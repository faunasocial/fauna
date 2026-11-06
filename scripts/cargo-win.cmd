@echo off
setlocal enabledelayedexpansion
REM Wrapper that sets up MSVC + LLVM environment and runs cargo.
REM Usage: scripts\cargo-win.cmd check -p fauna-sync-agent
REM
REM Host and target, both arches either way round: an arm64 dev box and the x64
REM hosted release runner run the same wrapper (release-integrity.md § What a
REM release ships → *Every release build runs the `just` recipe*).
REM   * The HOST is the arch rustc builds for itself (`rustc -vV`'s `host:` line),
REM     which picks the `bin\Host<host>\` tools. Never PROCESSOR_ARCHITECTURE: an
REM     emulated parent rewrites it for its children — Windows's Git Bash is an x64
REM     process and reports AMD64 on the arm64 box (measured 2026-10-28).
REM   * The TARGET is the `--target <triple>` (or `--target=<triple>`) on the
REM     command line for an `x86_64-` or `aarch64-pc-windows-msvc` triple; else
REM     CARGO_WIN_ARCH (`x64` | `arm64`); else the host. `--target` wins over the
REM     variable, so one leaked from a caller's environment cannot give a target
REM     build the other arch's libraries. It selects `bin\Host<host>\<target>` and
REM     the `lib\<target>` / SDK `Lib\...\{um,ucrt}\<target>` directories — the
REM     cross tools ship inside the same BuildTools/SDK install (the arm64 box's
REM     `Hostarm64\x64`, verified present 2026-08-24). The caller still passes
REM     `--target` to cargo; this wrapper only selects the matching environment.

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

REM rustup's proxies first, so the host probe below finds rustc on a shell that
REM has not put them on PATH yet.
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "HOST_ARCH=arm64"
rustc -vV 2>nul | findstr /b /c:"host: x86_64-" >nul && set "HOST_ARCH=x64"

set "TARGET_ARCH="
set "PREV_ARG="
for %%A in (%*) do (
    if "!PREV_ARG!"=="--target" (
        if "%%~A"=="x86_64-pc-windows-msvc" set "TARGET_ARCH=x64"
        if "%%~A"=="aarch64-pc-windows-msvc" set "TARGET_ARCH=arm64"
    )
    set "PREV_ARG=%%~A"
)
if not defined TARGET_ARCH if "%CARGO_WIN_ARCH%"=="x64" set "TARGET_ARCH=x64"
if not defined TARGET_ARCH if "%CARGO_WIN_ARCH%"=="arm64" set "TARGET_ARCH=arm64"
if not defined TARGET_ARCH set "TARGET_ARCH=!HOST_ARCH!"

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

REM The newest toolset that carries this host's tools for this target: a box
REM may keep an older toolset (a hosted runner's v142) without the cross tools.
set "MSVC_VER="
if exist "!VS_PATH!\VC\Tools\MSVC" (
    for /f "delims=" %%D in ('dir /b /ad /o-n "!VS_PATH!\VC\Tools\MSVC" 2^>nul') do (
        if not defined MSVC_VER if exist "!VS_PATH!\VC\Tools\MSVC\%%D\bin\Host!HOST_ARCH!\!TARGET_ARCH!\cl.exe" set "MSVC_VER=%%D"
    )
)
if "!MSVC_VER!"=="" set "MSVC_VER=%FALLBACK_MSVC_VER%"

REM The newest SDK version directory with this target's libraries and the CRT
REM headers — `10.*` only: the Driver Kit adds a `wdf` directory beside the
REM versions, which sorts first by name.
set "SDK_VER="
if exist "!SDK_PATH!\Lib" (
    for /f "delims=" %%D in ('dir /b /ad /o-n "!SDK_PATH!\Lib\10.*" 2^>nul') do (
        if not defined SDK_VER if exist "!SDK_PATH!\Lib\%%D\um\!TARGET_ARCH!\kernel32.lib" if exist "!SDK_PATH!\Include\%%D\ucrt\assert.h" set "SDK_VER=%%D"
    )
)
if "!SDK_VER!"=="" set "SDK_VER=%FALLBACK_SDK_VER%"

if "%CARGO_WIN_DEBUG_TOOLCHAIN%"=="1" (
    echo DEBUG VS_PATH=!VS_PATH!
    echo DEBUG MSVC_VER=!MSVC_VER!
    echo DEBUG SDK_VER=!SDK_VER!
    echo DEBUG HOST_ARCH=!HOST_ARCH!
    echo DEBUG TARGET_ARCH=!TARGET_ARCH!
    exit /b 0
)

REM Fail here, naming what was looked for, rather than a raw link.exe/cl.exe
REM error deep inside cargo's build output — a missing toolchain is common on
REM a fresh contributor machine and should say so plainly.
if not exist "!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!" (
    echo cargo-win.cmd: MSVC toolset not found at "!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!" ^(tried vswhere.exe discovery, then the pinned fallback %FALLBACK_MSVC_VER%^). Install Visual Studio / Build Tools with the "Desktop development with C++" workload, or set VS_PATH and MSVC_VER before calling this script.
    exit /b 1
)
if not exist "!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!\bin\Host!HOST_ARCH!\!TARGET_ARCH!" (
    echo cargo-win.cmd: no !HOST_ARCH!-hosted !TARGET_ARCH! tools at "!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!\bin\Host!HOST_ARCH!\!TARGET_ARCH!". Install the MSVC build tools for the !TARGET_ARCH! target.
    exit /b 1
)
if not exist "!SDK_PATH!\Lib\!SDK_VER!" (
    echo cargo-win.cmd: Windows SDK not found at "!SDK_PATH!\Lib\!SDK_VER!" ^(tried directory discovery, then the pinned fallback %FALLBACK_SDK_VER%^). Install the Windows 10/11 SDK, or set SDK_VER before calling this script.
    exit /b 1
)

set "PATH=!VS_PATH!\VC\Tools\MSVC\!MSVC_VER!\bin\Host!HOST_ARCH!\!TARGET_ARCH!;C:\Program Files\LLVM\bin;%PATH%"

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
