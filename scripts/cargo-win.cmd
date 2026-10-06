@echo off
setlocal enabledelayedexpansion
REM Wrapper that sets up MSVC + LLVM environment and runs cargo.
REM Usage: scripts\cargo-win.cmd check -p fauna-sync-agent
REM
REM Cross-arch: set CARGO_WIN_ARCH=x64 before calling to select the x64 cl/link/lib
REM environment for a --target x86_64-pc-windows-msvc cross-compile from this arm64
REM host (the `bin\Hostarm64\x64` cross tools + `lib\x64`/SDK `Lib\...\{um,ucrt}\x64`
REM ship inside the same BuildTools/SDK install already used for the native arm64
REM build — verified present 2026-08-24). Default (unset or any other value) is the
REM original arm64 host toolchain, unchanged. This wrapper only selects the matching
REM environment; the caller still passes --target x86_64-pc-windows-msvc to cargo.

REM ── Toolchain discovery (build-system.md § Windows toolchain
REM location): resolve VS_PATH/MSVC_VER/SDK_VER from THIS machine's own install
REM via vswhere.exe (a fixed, version-independent location on every VS/Build
REM Tools install since 15.2) and the newest versioned subdirectory under each
REM tool root, instead of pinning one box's exact build numbers — a contributor
REM whose install differs by even one build number would otherwise fail on step
REM one of the documented build (apps/fauna-windows/README.md). A value
REM discovery finds always wins; the FALLBACK_* constants below (this box's
REM last-known-good values) are used only when discovery itself cannot run
REM (no vswhere.exe, or an empty/missing tool directory) so this box's own
REM behaviour is unchanged either way.
REM
REM `setlocal enabledelayedexpansion` + `!VAR!` (not `%VAR%`) is load-bearing
REM inside every block below: every discovered path lives under
REM `...\Program Files (x86)\...`, and cmd.exe's block parser matches `(`/`)`
REM structurally — a `%VAR%` expanding to text containing `)` inside an
REM `if (...)`/`for (...)` block closes the block early ("was unexpected at
REM this time"). Delayed (`!VAR!`) expansion happens after the block is
REM already parsed, so the embedded parens in the VALUE never reach the parser.
set "FALLBACK_MSVC_VER=14.50.35717"
set "FALLBACK_VS_PATH=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools"
set "FALLBACK_SDK_VER=10.0.26100.0"
set "SDK_PATH=C:\Program Files (x86)\Windows Kits\10"

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VS_PATH="
if exist "!VSWHERE!" (
    for /f "usebackq delims=" %%P in (`"!VSWHERE!" -latest -products * -property installationPath`) do (
        set "VS_PATH=%%P"
        goto :vs_path_found
    )
)
:vs_path_found
if "!VS_PATH!"=="" set "VS_PATH=%FALLBACK_VS_PATH%"

set "MSVC_VER="
if exist "!VS_PATH!\VC\Tools\MSVC" (
    for /f "delims=" %%D in ('dir /b /o-n "!VS_PATH!\VC\Tools\MSVC" 2^>nul') do (
        set "MSVC_VER=%%D"
        goto :msvc_ver_found
    )
)
:msvc_ver_found
if "!MSVC_VER!"=="" set "MSVC_VER=%FALLBACK_MSVC_VER%"

set "SDK_VER="
if exist "!SDK_PATH!\Lib" (
    for /f "delims=" %%D in ('dir /b /o-n "!SDK_PATH!\Lib" 2^>nul') do (
        set "SDK_VER=%%D"
        goto :sdk_ver_found
    )
)
:sdk_ver_found
if "!SDK_VER!"=="" set "SDK_VER=%FALLBACK_SDK_VER%"

if "%CARGO_WIN_DEBUG_TOOLCHAIN%"=="1" (
    echo DEBUG VS_PATH=!VS_PATH!
    echo DEBUG MSVC_VER=!MSVC_VER!
    echo DEBUG SDK_VER=!SDK_VER!
    exit /b 0
)

REM Fail here, naming what was looked for, rather than a raw link.exe/cl.exe
REM error deep inside cargo's build output — a missing toolchain is common on
REM a fresh contributor machine and should say so plainly.
if not exist "!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!" (
    echo cargo-win.cmd: MSVC toolset not found at "!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!" ^(tried vswhere.exe discovery, then the pinned fallback %FALLBACK_MSVC_VER%^). Install Visual Studio / Build Tools with the "Desktop development with C++" workload, or set VS_PATH and MSVC_VER before calling this script.
    exit /b 1
)
if not exist "!SDK_PATH!\Lib\!SDK_VER!" (
    echo cargo-win.cmd: Windows SDK not found at "!SDK_PATH!\Lib\!SDK_VER!" ^(tried directory discovery, then the pinned fallback %FALLBACK_SDK_VER%^). Install the Windows 10/11 SDK, or set SDK_VER before calling this script.
    exit /b 1
)

if "%CARGO_WIN_ARCH%"=="x64" (
    set "TARGET_ARCH=x64"
) else (
    set "TARGET_ARCH=arm64"
)

set "PATH=!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!\bin\Hostarm64\!TARGET_ARCH!;C:\Program Files\LLVM\bin;%USERPROFILE%\.cargo\bin;%PATH%"

set "LIB=!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!\lib\!TARGET_ARCH!;!SDK_PATH!\Lib\!SDK_VER!\um\!TARGET_ARCH!;!SDK_PATH!\Lib\!SDK_VER!\ucrt\!TARGET_ARCH!"
set "INCLUDE=!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!\include;!SDK_PATH!\Include\!SDK_VER!\ucrt;!SDK_PATH!\Include\!SDK_VER!\um;!SDK_PATH!\Include\!SDK_VER!\shared"

set "CC=cl"
set "CXX=cl"

cargo %*
REM Propagate cargo's exit code through `cmd /c` (without this, a batch file's
REM status is unreliable when invoked from Git Bash as `cmd //c cargo-win.cmd …`,
REM and a FAILED cargo reported success — the documented exit-code lie every
REM caller had to work around by grepping output; the Windows merge script's test-code
REM compile gate is the first caller that must trust the code).
exit /b %ERRORLEVEL%
