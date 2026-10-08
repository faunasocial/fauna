import atexit
import contextlib
import datetime
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path

import pytest

# Self-terminating harness guard: pytest.ini's `timeout = 900` only binds when the
# pytest-timeout plugin is installed — without it, pytest merely warns about an
# unknown ini key and every test runs UNBOUNDED again (the 2026-07-13 wedge held a
# machine-wide lock for 10 h that way). Fail fast with the install command instead
# of silently reverting to the pre-incident behavior.
try:
    import pytest_timeout  # noqa: F401
except ImportError as _e:
    raise pytest.UsageError(
        "pytest-timeout is required (it enforces the harness-wide per-test bound "
        "that makes e2e runs self-terminating — testing.md § Cross-app e2e "
        "conventions, point 9). Install it into the e2e venv: "
        "`uv pip install --python <venv>/bin/python pytest-timeout` "
        "(see the per-machine setup notes, § e2e venv)."
    ) from _e

# Add tests/ to sys.path so `from common import ...` resolves to tests/common/.
# This is needed because e2e-unified has its own pytest.ini making it a separate
# rootdir, so the root tests/conftest.py won't be loaded automatically.
_tests_dir = str(Path(__file__).resolve().parent.parent)
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)

from common import (
    CLAIM_CODE,
    build_node,
    build_sync_service_win,
    start_nest,
    create_actor_and_register,
    get_repo_root,
)

_repo_root = get_repo_root()

sys.path.insert(0, str(Path(__file__).parent))
from drivers import create_driver  # noqa: E402
from actions import ActionLayer  # noqa: E402


# --- Platform detection ---

def detect_platform() -> str:
    """Returns: 'ubuntu', 'macos', 'windows', or 'github'."""
    if os.environ.get("GITHUB_ACTIONS"):
        return "github"
    system = platform.system()
    if system == "Linux":
        return "ubuntu"
    elif system == "Darwin":
        return "macos"
    elif system == "Windows":
        return "windows"
    raise RuntimeError(f"Unsupported platform: {system}")


def _android_available() -> bool:
    """Android available if THIS RUN's device is attached and the APK exists.

    The serial the run named (`--device-serial` / `E2E_DEVICE_SERIAL`) is part
    of the question, not just of the driving: a bare "some device is attached"
    probe followed by a `-s <serial>` driver is how a run on a box with two
    devices silently adopts a foreign one and reports its results as the named
    device's (`helpers/android_device.py` — *Why the serial must reach the
    availability probe*). `device_present` also refuses `unauthorized` /
    `offline`, which a driver would only discover at its first `adb install`.

    The adb server the run named (`--adb-server` / `E2E_ADB_SERVER`) is part of
    the question for the same reason: the probe asks the server the driver will
    drive. A venue run whose tunnel is down therefore answers False here —
    android unavailable, never red (`android_device.probe_device`).
    """
    from helpers import android_device

    if not shutil.which("adb"):
        return False
    if not android_device.probe_device(
        android_device.device_serial(), android_device.adb_server()
    ):
        return False
    return (_repo_root / APP_PATHS["android"]).exists()


def _find_simulator_udid() -> str | None:
    """Find an available iPhone simulator UDID."""
    try:
        result = subprocess.run(
            ["xcrun", "simctl", "list", "devices", "available", "--json"],
            capture_output=True, text=True, check=True,
        )
        devices = json.loads(result.stdout)["devices"]
        for runtime, devs in devices.items():
            for dev in devs:
                if "iPhone" in dev["name"] and dev["isAvailable"]:
                    return dev["udid"]
    except (FileNotFoundError, subprocess.CalledProcessError):
        pass
    return None


def _ios_derived_data_dir() -> Path:
    """Per-working-tree iOS derived-data path.

    The iOS app build (`xcodebuild` below) and the macOS-host's `just apple-ffi-test`
    static lib are both per-working-tree. A single shared `/tmp/fauna-e2e-build`
    would let two concurrent working trees corrupt each other's `xcodebuild`
    derived data AND cross-link the wrong per-working-tree FFI into the `.app`.
    Keying the path on the working-tree root (stable, so the incremental build
    cache survives across runs in the same working tree) removes that
    cross-working-tree collision — the build half of bringing iOS e2e to the
    per-instance isolation linux/macOS already have.
    """
    h = hashlib.sha1(str(_repo_root).encode()).hexdigest()[:10]
    return Path(f"/tmp/fauna-e2e-build-{_repo_root.name}-{h}")


def _find_ios_runtime_and_devicetype() -> tuple[str, str] | None:
    """Return (iOS-runtime-id, iPhone-device-type-id) for `simctl create`, or None."""
    try:
        rt = subprocess.run(
            ["xcrun", "simctl", "list", "runtimes", "--json"],
            capture_output=True, text=True, check=True,
        )
        runtimes = json.loads(rt.stdout)["runtimes"]
        ios_rt = next(
            (r for r in runtimes if r.get("isAvailable") and "iOS" in r.get("name", "")),
            None,
        )
        dt = subprocess.run(
            ["xcrun", "simctl", "list", "devicetypes", "--json"],
            capture_output=True, text=True, check=True,
        )
        devicetypes = json.loads(dt.stdout)["devicetypes"]
        iphone = next(
            (d for d in devicetypes if d.get("name") == "iPhone 17 Pro"), None
        ) or next(
            (d for d in devicetypes if "iPhone" in d.get("name", "")), None
        )
        if ios_rt and iphone:
            return ios_rt["identifier"], iphone["identifier"]
    except (FileNotFoundError, subprocess.CalledProcessError, KeyError,
            json.JSONDecodeError):
        pass
    return None


def _create_ephemeral_simulator() -> str | None:
    """Create a fresh, throwaway iOS simulator for this pytest session.

    This is the iOS analogue of linux/macOS's per-launch `mkdtemp` HOME/XDG
    isolation: linux/macOS run the app as a bare host process with a private
    state dir, so concurrent runs never collide. An iOS app can only run inside a
    simulator, and the simulator's unit of on-disk isolation is the *device*
    (the `(UDID, bundle-id)` container) — not a directory. So a per-session
    private *device* is the equivalent isolation. Two concurrent working trees
    now each get their own simulator → iOS e2e stops being a machine-wide singleton.

    Per-session (not per-launch) is sufficient: within one pytest session tests
    run sequentially and the driver's `uninstall`+`install` already wipes the
    container between launches; cross-session/cross-working-tree is the only
    axis that needed a separate device *for a single seat*. Two iOS apps
    launched *simultaneously* in one session need their OWN devices (row
    290) — `_get_ios_second_seat_udid` calls this same function again for
    the second seat, so callers needing more than one concurrently-live
    device just call it more than once; nothing here caps it at one.

    Deleted at process exit via `atexit`. Returns the new UDID, or None on any
    probe/create failure (caller falls back to a shared pre-existing sim).
    """
    info = _find_ios_runtime_and_devicetype()
    if not info:
        print("No available iOS runtime/device-type to create an ephemeral simulator")
        return None
    runtime, devicetype = info
    name = f"fauna-e2e-ios-{os.getpid()}"
    res = subprocess.run(
        ["xcrun", "simctl", "create", name, devicetype, runtime],
        capture_output=True, text=True,
    )
    if res.returncode != 0:
        print(f"simctl create failed (rc={res.returncode}): {res.stderr.strip()}")
        return None
    udid = res.stdout.strip()
    atexit.register(_delete_simulator, udid)
    return udid


def _delete_simulator(udid: str) -> None:
    """Shutdown + delete a throwaway simulator (best-effort, for atexit)."""
    subprocess.run(["xcrun", "simctl", "shutdown", udid],
                   check=False, capture_output=True)
    subprocess.run(["xcrun", "simctl", "delete", udid],
                   check=False, capture_output=True)


def _ios_package_workspace(package_dir: Path) -> Path:
    """Synthetic ``.xcworkspace`` whose sole ``FileRef`` is ``package_dir``, so
    ``xcodebuild -scheme FaunaiOS`` resolves the SPM package's own scheme.

    Without an explicit ``-workspace``/``-project``, xcodebuild auto-discovers
    a project in the given directory — and prefers a co-located ``.xcodeproj``
    over the implicit SPM-package scheme. ``apps/fauna-apple/Fauna.xcodeproj``
    (added by the File Provider M0 work) put exactly that in the same
    directory as ``Package.swift``, so a bare ``-scheme FaunaiOS`` has silently
    resolved against ``Fauna.xcodeproj`` since — which has no such scheme (only
    ``Fauna``/``Fauna-FileProvider``) — failing with "does not contain a scheme
    named FaunaiOS", which ``_build_ios_app`` swallows into a plain None →
    every iOS e2e test silently SKIPPED ("iOS setup not available") instead of
    reporting the real build break. An explicit workspace referencing only the
    package directory disambiguates regardless of what other project files
    that directory grows. Written fresh on every call (cheap, avoids staleness).
    """
    ws_dir = _ios_derived_data_dir().parent / f"{_ios_derived_data_dir().name}-pkg.xcworkspace"
    ws_dir.mkdir(parents=True, exist_ok=True)
    (ws_dir / "contents.xcworkspacedata").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<Workspace version = "1.0">\n'
        f'   <FileRef location = "group:{package_dir}">\n'
        '   </FileRef>\n'
        '</Workspace>\n'
    )
    return ws_dir


#: The `FaunaFFI.xcframework` slice an iOS-Simulator build links against.
_IOS_SIMULATOR_SLICE = "ios-arm64-simulator"


def _xcframework_slices() -> list[str]:
    """Slice directory names present in `apps/fauna-apple/FaunaFFI.xcframework`
    (``[]`` when the framework is absent).

    ONE path is written by two mutually destructive producers: `just apple-ffi-test`
    assembles the full slice set (darwin + ios + ios-sim; plus watchos +
    watchos-sim only on the explicit `just apple-ffi-watch` opt-in, 2026-08-22 —
    nothing links those, so the default set is 3),
    while `just apple-ffi-host-test` — a dependency of `just mac-debug` AND
    `just swift-test` — assembles the 1 darwin slice and `rm -rf`s whatever was
    there first (the justfile's `.ffi-flavor` guard). So a macOS build silently
    strips the iOS slices out from under an iOS build, and vice versa.
    """
    xcf = _repo_root / "apps" / "fauna-apple" / "FaunaFFI.xcframework"
    if not xcf.is_dir():
        return []
    return sorted(p.name for p in xcf.iterdir() if p.is_dir())


def _build_ios_app() -> str | None:
    """Build the iOS app for simulator testing. Returns the .app path.

    Returns ``None`` only when the apple package is genuinely absent. A build
    that FAILS raises — it must never degrade into the caller's
    ``skip_unbuilt("ios", ...)``, because a whole client leg reported as
    "skipped" reads downstream as *covered*. That exact swallow has
    now cost two separate incidents: the `Fauna.xcodeproj` scheme collision (see
    `_ios_package_workspace`) and the xcframework clobber this function now
    pre-checks. Both were invisible in the run summary — every iOS test simply
    said SKIPPED, and pytest hides a fixture's stdout for a skip, so even the
    "iOS build failed" print above never reached the log.
    """
    package_dir = _repo_root / "apps" / "fauna-apple"
    if not package_dir.exists():
        return None
    derived = _ios_derived_data_dir()

    # The clobber check, BEFORE spending an xcodebuild on a link that cannot
    # resolve. `_prebuild_binaries` restores the full framework after the macOS
    # app is built, so reaching here means that restore did not happen (or a
    # `just mac-debug`/`swift-test` ran afterwards) — say so, rather than let
    # xcodebuild's "no library for this platform" arrive with no cause attached.
    slices = _xcframework_slices()
    if slices and _IOS_SIMULATOR_SLICE not in slices:
        raise RuntimeError(
            f"FaunaFFI.xcframework has no {_IOS_SIMULATOR_SLICE!r} slice "
            f"(present: {slices}), so the iOS app cannot link it.\n"
            "Cause: `just apple-ffi-host-test` (a dep of `just mac-debug` and "
            "`just swift-test`) deletes a full xcframework and reassembles it "
            "host-only.\n"
            "Fix: run `just apple-ffi-test` LAST, after any macOS build:\n"
            f"    just apple-ffi-test    # from {_repo_root}"
        )

    result = subprocess.run(
        ["xcodebuild", "build",
         "-workspace", str(_ios_package_workspace(package_dir)),
         "-scheme", "FaunaiOS",
         "-destination", "platform=iOS Simulator,name=iPhone 17 Pro",
         "-derivedDataPath", str(derived),
         "-skipPackagePluginValidation", "-skipMacroValidation"],
        capture_output=True, text=True, cwd=package_dir,
    )
    if result.returncode != 0:
        lines = (result.stdout + result.stderr).strip().split("\n")
        tail = "\n".join(f"    {line}" for line in lines[-30:])
        raise RuntimeError(
            f"building the iOS app failed: xcodebuild exited "
            f"{result.returncode}.\n"
            f"--- last {min(len(lines), 30)} line(s) of build output ---\n"
            f"{tail or '    <no output captured>'}\n"
            f"--- end build output ---\n"
            f"Reproduce with:\n"
            f"    just apple-ffi-test && xcodebuild build -scheme FaunaiOS "
            f"-destination 'platform=iOS Simulator,name=iPhone 17 Pro'\n"
            f"(run it from {package_dir})."
        )

    # xcodebuild for SPM puts bare binary in Products — create .app bundle
    products = derived / "Build" / "Products" / "Debug-iphonesimulator"
    exe = products / "FaunaiOS"
    if not exe.exists():
        # xcodebuild reported success but produced nothing — a build-system
        # break, not an unavailable platform. Same rule as above: never a skip.
        raise RuntimeError(
            f"xcodebuild succeeded but the built executable is missing at {exe}"
        )

    app_dir = derived / "FaunaiOS.app"
    app_dir.mkdir(exist_ok=True)
    shutil.copy2(str(exe), str(app_dir / "FaunaiOS"))

    # Written UNCONDITIONALLY, and the id comes from the driver rather than a
    # second literal. Both halves matter, and the 2026-08-12 identifier sweep
    # proved why: `derived/` persists across runs, so an `if not exists` guard
    # kept serving a bundle stamped with the PREVIOUS id while the driver
    # launched the new one — `simctl install` succeeded (it installs whatever the
    # plist says) and `simctl launch` failed with FBSOpenApplicationServiceError
    # code=4 "The request to open ... failed", a message that names the app and
    # says nothing about a stale bundle. Deriving from `IOS_BUNDLE_ID` makes the
    # pair unable to disagree at all; `test_apple_identifier_pins.py` holds both
    # to the shipping `.xcodeproj` value.
    from drivers.ios import BUNDLE_ID as IOS_BUNDLE_ID

    info_plist = app_dir / "Info.plist"
    info_plist.write_text(f"""\
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>FaunaiOS</string>
    <key>CFBundleIdentifier</key><string>{IOS_BUNDLE_ID}</string>
    <key>CFBundleName</key><string>Fauna</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleSupportedPlatforms</key>
    <array><string>iPhoneSimulator</string></array>
    <key>MinimumOSVersion</key><string>17.0</string>
    <key>DTPlatformName</key><string>iphonesimulator</string>
    <key>UIDeviceFamily</key>
    <array><integer>1</integer><integer>2</integer></array>
    <key>UILaunchScreen</key><dict/>
    <key>NSPhotoLibraryUsageDescription</key>
    <string>Fauna backs up your photos to your personal node.</string>
    <key>NSAppTransportSecurity</key>
    <dict>
        <key>NSAllowsArbitraryLoads</key><true/>
    </dict>
</dict>
</plist>
""")

    return str(app_dir)


_WINDOWS_APP_BASE = "apps/fauna-windows/FaunaApp/FaunaApp/bin"
_WINDOWS_APP_SUFFIX = "Debug/net10.0-windows10.0.26100/FaunaApp.exe"

APP_PATHS = {
    "web": None,
    "android": "apps/fauna-android/app/build/outputs/apk/debug/app-debug.apk",
    "ios": "apps/fauna-apple/build/Build/Products/Debug-iphonesimulator/fauna-ios.app",
    "macos": "apps/fauna-apple/.build/arm64-apple-macosx/debug/FaunaMacOS",
    "windows": f"{_WINDOWS_APP_BASE}/{_WINDOWS_APP_SUFFIX}",
    "linux": "target/debug/fauna-desktop",
    "tui": "target/debug/fauna-tui",
}

def _resolve_windows_app() -> Path | None:
    """Find the Windows app exe — prefers ARM64/x64 over AnyCPU since the platform-specific
    builds are typically newer (MSBuild on ARM64 builds to bin/ARM64/Debug). Among matches,
    return the most recently modified to avoid using stale binaries."""
    candidates = []
    for sub in ["ARM64", "x64", ""]:
        p = _repo_root / _WINDOWS_APP_BASE / sub / _WINDOWS_APP_SUFFIX
        if p.exists():
            candidates.append(p)
    if not candidates:
        return None
    return max(candidates, key=lambda p: p.stat().st_mtime)


# MSBuild path for WinUI XAML builds (dotnet build crashes the XAML compiler on ARM64).
_MSBUILD = Path(r"C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\MSBuild\Current\Bin\MSBuild.exe")
_WINDOWS_CSPROJ = _repo_root / "apps" / "fauna-windows" / "FaunaApp" / "FaunaApp" / "FaunaApp.csproj"


def _build_windows_app() -> Path:
    """Build the Windows app if the exe doesn't exist. Returns the exe path.

    Uses MSBuild (not dotnet build) because the WinUI XAML compiler crashes
    under x86 emulation on ARM64 when invoked out-of-process by dotnet build.
    Falls back to dotnet build if MSBuild is not found (x64 machines).
    """
    existing = _resolve_windows_app()
    if existing is not None:
        return existing

    # Restore NuGet packages first (MSBuild needs project.assets.json)
    restore = subprocess.run(
        ["dotnet", "restore", str(_WINDOWS_CSPROJ)],
        capture_output=True, text=True, timeout=120,
    )
    if restore.returncode != 0:
        raise RuntimeError(f"dotnet restore failed:\n{restore.stderr[-1000:]}")

    if _MSBUILD.exists():
        cmd = [str(_MSBUILD), str(_WINDOWS_CSPROJ),
               "-p:Configuration=Debug", "-p:Platform=ARM64",
               "-verbosity:minimal"]
    else:
        cmd = ["dotnet", "build", str(_WINDOWS_CSPROJ), "-c", "Debug"]

    print(f"[windows] App not found, building with {'MSBuild' if _MSBUILD.exists() else 'dotnet build'}...")
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    if result.returncode != 0:
        raise RuntimeError(
            f"Windows app build failed (exit {result.returncode}):\n"
            f"{result.stderr[-1000:]}\n{result.stdout[-1000:]}"
        )

    built = _resolve_windows_app()
    if built is None:
        raise RuntimeError(
            f"Windows app build succeeded but exe not found. "
            f"Searched {_WINDOWS_APP_BASE}/[ARM64|x64|]/{_WINDOWS_APP_SUFFIX}"
        )
    print(f"[windows] App built: {built}")
    return built


# The rust-first default app (testing.md § Default app and nest mode).
DEFAULT_APP = "tui"
# The `--app` token that expands to this machine's full sweep set.
SWEEP_TOKEN = "sweep"


def sweep_apps() -> list[str]:
    """The machine's full sweep set — every app this box can actually drive.

    Until the rust-first flip (2026-08-01) this WAS the default set. The flip
    made `[tui]` the default on every dev machine and demoted these to the
    **sweep sets**: what `--app sweep` expands to, and what a cross-app parity
    or trickle-down pass runs (`testing.md` § Default app and nest mode).

    They live in code rather than in a per-machine doc table so that
    `--app sweep` means the right thing on every dev machine with nobody
    re-typing a list — a table is one more thing to drift.
    """
    plat = detect_platform()
    apps: list[str] = []
    if plat == "ubuntu":
        apps.append("web")
        # linux is auto-built by _ensure_app_built on first request,
        # so include unconditionally — first run pays ~30s cargo build,
        # subsequent runs are cheap.
        apps.append("linux")
        # tui joined the defaults at the M9 parity flip (2026-07-19,
        # tui.md § Rollout) — cross-platform, auto-built like linux.
        apps.append("tui")
    elif plat == "macos":
        # ⚠ **No web leg on macOS, and this is measured, not a preference.**
        # `fauna-core` depends unconditionally on `zstd`, whose `zstd-sys` build
        # script compiles C for `wasm32-unknown-unknown` through `cc`/clang — and
        # Apple clang ships NO WebAssembly backend (`clang -print-targets` lists
        # none). There is no other LLVM on the box and Homebrew is deliberately
        # absent (supply-chain risk), so `just wasm-core-test` cannot be made to
        # pass here — the macOS dev-setup notes have carried the finding since
        # 2026-06-18, re-measured 2026-07-29.
        #
        # Naming web here anyway did not merely overstate the set — it broke the
        # token outright: `_prebuild_web_spa` runs at COLLECTION, so `--app sweep`
        # on macOS died in the wasm build before a single test of ANY module ran. This function's own docstring is the reason
        # that went unnoticed for so long: it promises "every app this box can
        # actually drive" and argues the set lives in code so a doc table cannot
        # drift from it — so a reader hitting the wall trusted the code and
        # doubted their box. The web leg runs on Linux alone — web is built
        # only there (build-system.md § The Deno build sandbox).
        if shutil.which("xcodebuild"):
            apps.append("ios")
        if (_repo_root / APP_PATHS["macos"]).exists():
            apps.append("macos")
        if _android_available():
            apps.append("android")
        apps.append("tui")
    elif plat == "windows":
        # No web leg: web is built only on Linux (build-system.md § The Deno
        # build sandbox, owner-ruled 2026-10-05).
        apps.append("windows")  # auto-built on demand in _build_app_config
        apps.append("tui")
    elif plat == "github":
        # GitHub Actions. NOTE (verified 2026-07-22): no current workflow
        # collects client-parametrized tests, so this list is consulted by
        # NOTHING today — tier4-deploy-e2e.yml targets tests/platform/docker,
        # a client-independent dir (_CLIENT_INDEPENDENT_DIRS), always
        # deselected under --client; the release workflows (build-nest-image.yml,
        # restage-nest.yml) run no pytest at all.
        # It declares what would run if such a workflow appeared. tui stays
        # out until a real validation pass proves it — impossible today: the
        # self-hosted runner has docker + python only (no rust toolchain), so
        # it can build neither fauna-tui nor the local nest_instance binaries
        # a tui journey needs. Revisit only alongside a workflow that both
        # collects client-parametrized tests and installs the toolchain.
        apps.append("web")
        apps.append("linux")
    return apps


def _resolve_app_tokens(raw: str) -> list[str]:
    """Split an `--app`/`E2E_APPS` value, expanding the `sweep` token.

    `sweep` expands in place to this machine's full set, so `--app sweep` and
    `--app sweep,android` both work and neither has to know which box it is
    on. Order is preserved; duplicates collapse.
    """
    out: list[str] = []
    for token in raw.split(","):
        token = token.strip()
        if not token:
            continue
        for app in (sweep_apps() if token == SWEEP_TOKEN else [token]):
            if app not in out:
                out.append(app)
    if "web" in out and detect_platform() not in ("ubuntu", "github"):
        # Refused here, before `_prebuild_web_spa` spends a wasm build that the
        # Deno sandbox launcher would then refuse anyway.
        raise pytest.UsageError(
            "--app web: web is built only on Linux, never on this machine "
            "(build-system.md § The Deno build sandbox) — run the web leg on the Linux dev machine"
        )
    return out


def get_available_apps() -> list[str]:
    """Which apps this run drives.

    The default is `[tui]` on every dev machine — the rust-first inner loop
    (`testing.md` § Default app and nest mode; ratified 2026-07-29, flipped
    2026-08-01). Cross-app coverage is therefore a deliberate, named act:
    pass `--app sweep` for this machine's full set (`sweep_apps()` above), or
    name the apps outright. That is the point of the flip rather than a cost
    of it — the goal doc's "the flip makes sweeps load-bearing".

    Override with --app (comma-separated), e.g. pytest --app ios
    Or set E2E_APPS env var. `sweep` is a valid token in either.

    `--client` / `E2E_CLIENTS` remain accepted aliases: the 2026-07-29
    clients→apps rename kept them working so the fleet's documented
    commands (and frozen docs) do not break. "app" is canonical.
    """
    # --app/--client CLI option (parsed from sys.argv because this runs at
    # import time, before pytest_addoption has been called).
    #
    # LAST occurrence wins, deliberately: that is what pytest's own
    # `config.getoption("--app")` returns, and the deselect hook reads it from
    # there. A first-wins scan here would disagree with the hook the moment a
    # `just` recipe supplies a default that the caller overrides
    # (`just e2e-version-skew-test --app linux` → `pytest … --app sweep --app
    # linux`): parametrization would build one app set and the filter would
    # select another.
    chosen = None
    for i, arg in enumerate(sys.argv):
        if arg in ("--app", "--client") and i + 1 < len(sys.argv):
            chosen = sys.argv[i + 1]
        elif arg.startswith("--app=") or arg.startswith("--client="):
            chosen = arg.split("=", 1)[1]
    if chosen is not None:
        return _resolve_app_tokens(chosen)
    override = os.environ.get("E2E_APPS") or os.environ.get("E2E_CLIENTS")
    if override:
        return _resolve_app_tokens(override)
    if detect_platform() == "github":
        # Not a dev machine, and consulted by NOTHING today (see sweep_apps).
        # The rust-first default is about the dev inner loop, and tui cannot
        # be built there at all, so github keeps declaring its own set.
        return sweep_apps()
    return [DEFAULT_APP]


# --- Binary ensure layer (memoized; shared by fixtures + collection prebuild) ---
#
# Every builder here runs at most once per pytest process, whichever door it
# enters through — `_prebuild_binaries` at collection time (the normal path)
# or a session fixture (the correctness fallback). The memo is what makes the
# collection-time hoist effective: without it a fixture re-running `just`
# would re-acquire the machine-wide build slot even on a fully warm tree,
# because `{{slot_build}}` wraps the cargo call unconditionally — and that
# wait would land back inside the per-test `timeout = 900`
# (build-system.md § Build/e2e slot locks; testing.md § point 9).

_BINARY_BUILD_MEMO: dict[str, tuple[str, object]] = {}


def _memoized_build(key: str, builder):
    """Run `builder()` once; replay its result — or its FAILURE — afterwards.

    The failure replay is deliberate: a build that failed at collection time
    must surface in the requesting fixture as the original error, not trigger
    a silent rebuild that re-queues the build slot inside the test's own
    timeout budget (the exact bound inversion this layer removes). Failure
    output already streamed to the log when the build ran.
    """
    state = _BINARY_BUILD_MEMO.get(key)
    if state is not None:
        kind, value = state
        if kind == "ok":
            return value
        raise RuntimeError(
            f"{key} already failed to build earlier in this run "
            f"(see the build output above): {value!r}"
        )
    try:
        value = builder()
    except Exception as exc:
        _BINARY_BUILD_MEMO[key] = ("failed", exc)
        raise
    _BINARY_BUILD_MEMO[key] = ("ok", value)
    return value


def _ensure_generated_files_fresh() -> None:
    """i18n strings + provider registry regenerated from yaml sources — the
    module-level twin of the `_generated_files_fresh` fixture (which delegates
    here). Cheap when sources are unchanged (~50ms via build-if-stale gates)."""

    def build():
        subprocess.run(
            ["just", "i18n-generate", "providers-generate"],
            cwd=_repo_root, check=True,
        )

    _memoized_build("generated-files", build)


def _ensure_nest_built() -> str:
    """Debug fauna-nest with the default e2e feature set; returns binary path."""

    def build():
        _ensure_generated_files_fresh()
        return build_node()

    return _memoized_build("nest-binary", build)


def _ensure_bluesky_nest_built() -> str:
    """fauna-nest WITH `bluesky`, copied to its own filename; returns the copy's
    path (see the `bluesky_nest_binary` fixture docstring for why it is copied)."""

    def build():
        _ensure_generated_files_fresh()
        built = Path(build_node(features="test-hooks,nostr,bluesky"))
        pinned = built.with_name(built.stem + "-bluesky" + built.suffix)
        shutil.copy2(built, pinned)
        return str(pinned)

    return _memoized_build("bluesky-nest-binary", build)


def _ensure_bridges_nest_built() -> str:
    """fauna-nest with ONLY the `activitypub` provider (+ test-hooks); returns
    binary path. `tests/test_bridges.py`'s dedicated provider-set build — see
    that module's `bridges_nest_binary` fixture docstring for why it needs its
    own variant rather than the default `nest_binary`."""

    def build():
        _ensure_generated_files_fresh()
        return build_node(features="test-hooks,activitypub")

    return _memoized_build("bridges-nest-binary", build)


def _ensure_ap_nest_built() -> str:
    """fauna-nest with `activitypub` + `test-hooks` (`helpers.ap_nest`'s
    federation-suite build, the `ap_binary` fixture); returns binary path. It
    used to be a bare in-test `cargo build` holding no slot."""

    def build():
        _ensure_generated_files_fresh()
        from helpers.ap_nest import build_ap_nest_binary

        return build_ap_nest_binary()

    return _memoized_build("ap-nest-binary", build)


def _ensure_sync_agent_built() -> str:
    """fauna-sync-agent.exe (windows Service-wrapped sync agent), once per
    session; `sync_agent_binary`'s builder. Was the last fixture in this
    layer left "bare cargo-win, no slot involved" — now routed through
    `common.nest.build_sync_service_win`'s gate-then-slot composition,
    same class as the nest builders above."""

    def build():
        _ensure_generated_files_fresh()
        return build_sync_service_win()

    return _memoized_build("sync-agent-binary", build)


def _ensure_fauna_ffi_loaded() -> None:
    """Load `fauna_ffi`'s cdylib once per session — building it if absent.

    The build lives in the module itself (`fauna_ffi._find_cdylib`, at import),
    but the headless seeding helpers import the module LAZILY, from inside a
    test body (`ApiActor.subscription_create_tier`, `helpers/enrollment.py`), so
    without this the first such test paid the cold cargo build and its
    machine-wide `build`-slot wait inside its own 900 s budget. Windows builds
    here too since 2026-09-28 (`just windows-ffi-test dev`, the flavor its
    loader now builds before mapping). A failure is the loader's own,
    re-raised so the prebuild log names it; the tests that call a builder
    still get it at first use.
    """

    def load():
        import fauna_ffi

        if isinstance(fauna_ffi._lib, fauna_ffi._UnavailableLib):
            raise RuntimeError(f"fauna-ffi unavailable: {fauna_ffi._lib._error}")

    _memoized_build("fauna-ffi-cdylib", load)


def _ensure_fauna_ffi_built() -> None:
    """The compile half of `_ensure_fauna_ffi_loaded`, split out so the warm
    pass can run it holding no e2e lane: the same recipe the loader runs at
    import (`fauna_ffi._build_via_just_e2e_ffi` → `just e2e-ffi release`),
    build-if-stale-gated outside `{{slot_build}}`, so the loader's own call
    inside the hold is a stamp read. Run through `just` rather than by
    importing `fauna_ffi`, whose import IS the load — the one-shot side effect
    the warm pass must leave to the hold. The profile is the loader's
    `_E2E_FFI_PROFILE` (pinned by `test_e2e_binary_prebuild.py`). Windows is
    skipped like the loader: `just windows-ffi` builds there, not this recipe.
    """
    if _IS_WINDOWS:
        return

    def build():
        subprocess.run(["just", "e2e-ffi", "release"], cwd=_repo_root, check=True)

    _memoized_build("fauna-ffi-cdylib-build", build)


# --- Fixtures ---

@pytest.fixture(scope="session")
def _generated_files_fresh():
    """Ensure i18n strings + provider registry are regenerated from yaml sources.

    Build fixtures that bypass just (e.g. nest_binary calls cargo directly) must
    depend on this so they don't compile against stale generated code. Cheap when
    sources are unchanged (~50ms total via build-if-stale gates). Delegates to
    the memoized module-level ensurer so the collection-time prebuild and this
    fixture share one run.
    """
    _ensure_generated_files_fresh()


@pytest.fixture(scope="session")
def nest_binary(_generated_files_fresh):
    """Build fauna-nest once per session.

    Normally a memo replay: `_prebuild_binaries` already built it at collection
    time, outside every per-test timeout budget.

    **REFUSES in a mode whose nest is not a local binary** (docker's is in the
    image, live's is on the remote box — `nest_mode.builds_local_nest`), and the
    refusal is the point rather than a side effect. `nest_instance` no longer
    declares this fixture — it resolves it lazily, and only when the mode
    actually needs it — so the only things left here are tests that want the
    *binary itself*, to stand up a nest of their own. In a docker or live run
    such a test would serve a **locally-built** nest while the report said
    docker/live: the silent fallback `NestModeError` exists to refuse.

    Those tests are precisely the ones `nest_surface.LOCAL_NEST_FIXTURES` cannot
    see — it classifies by conftest fixture, and these spawn their nest in their
    own module — so this is where that hole is closed, loudly and with the fix in
    the message.
    """
    from helpers import nest_mode as nest_mode_mod

    mode = nest_mode_mod.run_mode()
    if not nest_mode_mod.builds_local_nest(mode):
        raise nest_mode_mod.NestModeError(
            f"a test asked for the locally-built fauna-nest binary in "
            f"{mode.name!r} mode, whose nest is not a local binary at all "
            f"(docker's is in the image, live's is on the remote box). Building "
            f"and spawning one here would serve a LOCAL nest while this run "
            f"reports {mode.name!r} — the silent fallback the nest-mode axis "
            f"refuses. Mark the test @pytest.mark.standalone_only so it is "
            f"excluded from the modes that cannot honour it (testing.md "
            f"§ Default app and nest mode)."
        )
    return _ensure_nest_built()


@pytest.fixture(scope="session")
def bluesky_nest_binary(_generated_files_fresh):
    """Build fauna-nest WITH the `bluesky` feature, once per session.

    The ATProto full-PDS *write* surface is feature-gated: the
    `fauna.bridges.atproto.ingest_external_write` kind only registers under
    `#[cfg(feature = "bluesky")]` (`bins/fauna-nest/src/bridge_atproto_handlers.rs`),
    because it pulls in the `fauna-bridge-atproto` crate. The default
    `nest_binary` deliberately omits it (every feature-gated provider adds
    startup surface the other tests don't want), so a test that drives an
    external ATProto write must take THIS binary — against the default one the
    write fails as an unknown kind, which reads like a routing bug rather than a
    missing feature.

    The read/projection surface is not gated, so the S-chain tier_3 tests
    (`test_atproto_firehose_post.py`, `test_atproto_pds_auth.py`) keep using
    plain `nest_binary`.

    The build is COPIED to its own filename. Both fixtures compile to the same
    `target/<profile>/fauna-nest` path, so in a session that instantiates both,
    whichever built last would own the file — and if that were `nest_binary`,
    this fixture's already-returned path would silently point at a nest with no
    `bluesky` feature, failing the write as an unknown kind. Copying makes the
    two binaries independent of fixture ordering.
    """
    return _ensure_bluesky_nest_built()


@pytest.fixture(scope="session")
def bench_nest_binary(_generated_files_fresh):
    """Build the RELEASE fauna-nest once per session — for perf measurement only.

    Every functional test uses the debug `nest_binary` (fast to compile). A perf
    benchmark must not: a debug nest's absolute round-trip latency and its
    mailbox-scaling behaviour are debug-amplified and NOT release-representative
    (understating the crypto-delta *fraction*, exaggerating scaling walls). The
    Phase-3 IMAP FETCH-latency benchmark (`test_imap_fetch_latency_bench.py`,
    `FAUNA_BENCH`-gated) stands its dedicated bench nests up on this release
    binary so the measured serve-path fraction is sound."""
    return build_node(release=True)


_WEB_SPA_BUILT = False


def _ensure_web_spa_built():
    """Build the SPA at most once per pytest session.

    MUST stay callable from `pytest_collection_finish` (outside every per-test
    pytest-timeout budget) — see `_prebuild_web_spa` for why that placement is
    load-bearing rather than an optimisation. Idempotent, so the `static_dir`
    fixture can call it too and get a no-op on the second call.
    """
    global _WEB_SPA_BUILT
    if _WEB_SPA_BUILT:
        return
    subprocess.run(["just", "web-test"], cwd=_repo_root, check=True)
    _WEB_SPA_BUILT = True


_WASM_PANIC_WITNESS_BUILT = False


def _ensure_wasm_panic_witness_built():
    """Build the wasm panic-hook headless-witness bundles at most once per
    session (`tests/test_wasm_panic_hook.py`'s own prerequisite).

    Same `pytest_collection_finish`-placement requirement as
    `_ensure_web_spa_built` — a `wasm-pack build` per chunk takes a machine-wide
    `build` slot, which must not be charged to that test's own per-test
    pytest-timeout budget (see `_prebuild_web_spa`'s docstring for the full
    bound-inversion story). Idempotent.
    """
    global _WASM_PANIC_WITNESS_BUILT
    if _WASM_PANIC_WITNESS_BUILT:
        return
    subprocess.run(["just", "wasm-panic-witness"], cwd=_repo_root, check=True)
    _WASM_PANIC_WITNESS_BUILT = True


@pytest.fixture(scope="session")
def static_dir():
    """Path to the built Svelte SPA. Uses `just web-test` so the bundled
    fauna-wasm-onboarding includes the test-helpers feature (the e2e
    bridge needs setHandleCheckSnapshotForTest / setInviteRequestSnapshotForTest
    / setStepForTest per the E2E bridge contract, tracked internally).
    Production builds use `just web` and don't ship those setters.

    Skips when the web app isn't in the active client set — `just web-test`
    requires a wasm32-capable toolchain that not every dev machine has, and
    macos/linux/windows/ios/android tests don't need the SPA. Tests that
    truly need the SPA (web app, `admin_app`, `spa_url`) will fail-fast
    with this skip rather than blocking the session on a wasm build that
    can't succeed.

    The build itself normally already happened in `pytest_collection_finish`
    (`_prebuild_web_spa`), so the call here is a no-op; it stays as the
    fallback that keeps this fixture correct on its own.
    """
    if "web" not in get_available_apps():
        pytest.skip("static_dir requires the web client (`just web-test` build)")
    _ensure_web_spa_built()
    return str(_repo_root / "apps" / "fauna-web" / "build")


def browser_origins_to_allow(request) -> list[str]:
    """The browser origins a nest this run's apps dial **raw** must allow.

    Empty unless the web app is in the active set: only a browser enforces CORS,
    so on a native-only run there is no origin to seed and nothing to resolve —
    which also keeps `spa_url` (and through it `static_dir`, which `pytest.skip`s
    when web isn't built) out of a native run's fixture closure.

    With web active it is the session SPA proxy's own origin, because that proxy
    IS the page the wasm client runs in (`_build_app_config`: web launches at
    `spa_url + "/app/"`). Requests the SPA makes through its own proxy are
    same-origin and need nothing; this exists for the nests the client dials
    round the proxy — see `common.nest.start_nest`'s `cors_origins`.

    Seeding the *test's* origin is not a weakening of the control, it is the
    substitution production already makes for itself: a shipped nest allows
    `https://app.fauna.social` because that is where the wizard is served from
    (`front-door.md` § the canonical hosted origin), and the harness's wizard is
    served from this proxy. The nest still makes a real, exact-match CORS
    decision against a real allow-list — the mechanism under test is preserved,
    not bypassed, which a proxy in front of the nest would not have done.
    """
    if "web" not in get_available_apps():
        return []
    return [request.getfixturevalue("spa_url")]


def _start_dedicated_nest(request, nest_mode, tmp_path_factory, label,
                          binary="nest_binary", **options):
    """Start a nest of this run's MODE. Returns `(nest_dict, cleanup_fn)`.

    The one seam every dedicated-nest fixture goes through, so "a second nest"
    is the mode's question and not each fixture's (`testing.md` § Default app
    and nest mode, ruling (1) — every nest the harness starts is the mode
    provider's to start). In standalone the provider *is* `_make_nest`, so the
    default inner loop is byte-identical and pays nothing; in docker it is
    another container on the run's network; in live it refuses, because a start
    option — and a start at all — is a property of a nest the harness owns.

    The binary is resolved **lazily**, the way `nest_instance` resolves it and
    for the same reason: docker's binary is in the image and live's is on the
    remote box, where `nest_binary` refuses outright. Taking it as a fixture
    parameter instead — which every caller of this helper used to do — drags
    that refusal into the setup of every test in the closure, and is itself the
    signal that excluded these fixtures from docker in the first place.

    ``binary`` names the build fixture standalone resolves — `nest_binary` by
    default, or a bespoke build for a test that needs a provider the default
    build does not compile (`bluesky_nest_binary`). It is a NAME and not a
    start option: it chooses the artifact, which a non-local mode already
    chose (the run's image), so it never reaches the provider. A non-local mode
    still checks it, because the image can stand in for a named build only
    when it ships that build's subject — `IMAGE_SERVABLE_BINARY_FIXTURES`. A
    class (9) build is refused here, never silently replaced (ruling (1);
    `nest_surface.DEDICATED_NEST_BINARIES` is the table this name is pinned to).
    """
    from helpers import nest_mode as nest_mode_mod
    from helpers import nest_surface as ns

    provider = nest_mode_mod.provider_for(nest_mode)
    if nest_mode_mod.builds_local_nest(nest_mode):
        nest_binary = request.getfixturevalue(binary)
    else:
        if binary != "nest_binary" and binary not in ns.IMAGE_SERVABLE_BINARY_FIXTURES:
            raise nest_mode_mod.NestModeError(
                f"{label!r} names the build {binary!r}, which the "
                f"{nest_mode!r} nest's image cannot stand in for: only a build "
                "listed in IMAGE_SERVABLE_BINARY_FIXTURES is servable by the "
                "image (testing.md § Default app and nest mode, ruling (1))"
            )
        nest_binary = None
    return provider.start(nest_binary, tmp_path_factory, label, **options)


def _start_mail_venue(request, nest_mode, tmp_path_factory, label, **options):
    """Start a MAIL VENUE of this run's mode. Returns `(handle, cleanup)`.

    Arm 6's seam, and it is a provider **METHOD** rather than a start option for
    a reason worth keeping: a mail venue's mail listeners must be published at
    container START, and `FIXTURE_START_OPTIONS` is name-level and
    mode-independent — it records the kwargs a fixture literally passes,
    identically in every mode — so a need that exists in docker and not in
    standalone is precisely what that table cannot express. `start_in_place`
    exists for the same reason and says it the same way: *the provider that knows
    how to START this nest also answers how to start it AGAIN.* Here: the
    provider that knows how to start this nest also answers what a mail nest IS
    in its mode — host-spawned bridges beside a binary, or the image's own s6
    services with their ports published.

    The two venues answer one contract (`MailVenueHandle`), so a consuming test
    reads the same attributes and calls the same methods either way; what differs
    is only who runs the bridges. `testing.md` § Default app and nest mode,
    ruling (3).

    Binaries are resolved **lazily** through `request`, exactly as
    `_start_dedicated_nest` resolves `nest_binary` and for the same reason:
    taking `mail_bridge_binary`/`seal_helper_binary`/`nest_binary` as fixture
    parameters is what put every one of these fixtures in the binary closure in
    the first place.
    """
    from helpers import nest_mode as nest_mode_mod

    provider = nest_mode_mod.provider_for(nest_mode)
    return provider.start_mail_venue(request, tmp_path_factory, label, **options)


def _make_nest(nest_binary, tmp_path_factory, label="nest", unclaimed=False,
               claim_domain=None, handle_domain_seed=None, serve_tls=False,
               dial_host=None, extra_env=None, cors_origins=None,
               static_dir=None):
    """Start a nest instance. Returns (nest_dict, cleanup_fn).

    ``unclaimed=True`` leaves the nest never-claimed (admin is None) so a
    client-UI onboarding e2e can drive the claim itself; the claim code is on
    disk at ``<nest['tmp_dir']>/claim-code``.

    ``serve_tls=True`` makes this nest serve its always-live SELF-SIGNED floor
    cert over real HTTPS on its API listener, by dropping the process-wide
    ``FAUNA_INSECURE_DISABLE_TLS`` plain-HTTP escape for this nest only
    (``common.nest._spawn_and_wait``). The returned ``url`` is then ``https://…``.
    See ``nest/domains-and-tls-bootstrap.md`` § Test posture.

    ``dial_host`` sets the authority clients dial (``common.nest.start_nest``).
    Default loopback; pass ``common.nest.lan_ipv4()`` for a NON-loopback authority,
    which is what drives a client's SPKI-**pin** trust branch rather than its
    loopback short-circuit.

    ``claim_domain`` claims this nest ONTO that domain — the claim carries it
    as ``mail_domain``, so the nest registers it as the primary ``mail_domains``
    row and that row becomes the deployment identity
    (``account_core::handle_domain``), which the client uses as its mail ``From``
    domain (``<handle>@<domain>``). Mirrors production, where the node domain,
    the handle domain and the mail domain are one and the same — and, being a
    wire act rather than a boot flag, it is honoured in **every** nest mode.

    ``handle_domain_seed`` is the older ``--handle-domain`` boot seed, kept for
    the two shapes no claim can express: an IP-literal **authority**
    (``127.0.0.1:<port>`` — the claim gate registers no local target) and an
    ``unclaimed=True`` nest whose claim is not the harness's to make. It is
    standalone-only, a class (4) declared absence everywhere else.

    For the IP-literal shape, pass ``common.nest.OWN_DIAL_AUTHORITY`` rather than
    a composed string. The authority is a fact about a nest that does not exist
    yet, so a caller composing it had to allocate the port ITSELF — and a caller
    that picks the port cannot let a provider pick it, which is exactly what kept
    the four such fixtures spawning their own binaries until 2026-09-02.

    ⚠ The second shape is about WHOSE claim it is, not about the nest being
    unclaimed. A test that drives its own claim can carry the domain there
    instead — ``tests/api/test_onboarding.py`` does exactly that, which is what
    let it route — so the seed survives only where the claim belongs to the app
    under test (the onboarding-UI journeys) or to nobody at all.

    ⚠ A fixture that registers its own domain over
    ``fauna.bridges.add_local_domain`` passes NEITHER: that call already sets the
    identity, and pre-registering at claim would make it an idempotent no-op that
    silently drops its MTA-STS / catch-all / DKIM arguments. See
    ``testing.md`` § Default app and nest mode, ruling (3).

    ``cors_origins`` seeds the browser origins this nest allows cross-origin
    (``--cors-origin``; ``common.nest.start_nest`` carries the full story). Boot
    wiring the deployment artifact sets, never a knob — the live surface stays
    ``fauna.admin.set_cors_origins``. Only a nest a BROWSER dials **raw** needs
    it: most web tests go through ``_serve_spa_proxy`` and are same-origin, so
    they seed nothing. Pass ``browser_origins_to_allow(request)``, which is empty
    on a run with no web app in the set.

    ``static_dir`` is the SPA build this nest serves (``--static-dir``): its
    ``/app/`` and the private share link viewer page ``GET /share/<token>``
    answers. Boot wiring the deployment artifact sets — the image points it at
    its bundled build — so only a test whose BROWSER loads a nest-served page
    needs it; pass the ``static_dir`` fixture.

    ``extra_env`` sets bucket-2 IPC env on this nest only — artifact-set wiring
    the deployment writes, never a human-edited knob (``common.nest.start_nest``
    carries the same parameter and the same rule). The box-recovery case is
    ``{"FAUNA_DEPLOYMENT_SEED": "<64-hex>"}``; the crash beacon's is the log
    level. Every mode honours it, which is the point of it living here rather
    than only on the caller that first needed it: in docker it is container env,
    reaching the nest because the s6 run scripts are ``with-contenv``.

    The registration posture is deliberately NOT a parameter here. Opening
    self-service ``fauna.account.register`` is an admin choice, so a caller that
    wants it calls ``common.auth.set_registration_mode(nest["port"],
    admin_signing_key=nest["admin"]["signing_key"])`` after this returns —
    mode-agnostically, over the same kind an admin's app uses (``testing.md``
    § Default app and nest mode, ruling (3)).
    """
    from drivers.port_util import find_free_port
    from helpers import android_venue
    tmp_dir = str(tmp_path_factory.mktemp(label))
    # `find_free_port()` in every run but an android venue run, where the port
    # is leased from the range the venue's tunnel carries — the only ports the
    # device can reach (`helpers/android_venue.nest_port`).
    port, release_port = android_venue.nest_port(find_free_port)
    try:
        nest = start_nest(nest_binary, tmp_dir, port=port, unclaimed=unclaimed,
                          claim_domain=claim_domain,
                          handle_domain_seed=handle_domain_seed,
                          serve_tls=serve_tls, dial_host=dial_host,
                          extra_env=extra_env, cors_origins=cors_origins,
                          static_dir=static_dir)
    except BaseException:
        release_port()
        raise

    def cleanup():
        nest["proc"].terminate()
        try:
            nest["proc"].wait(timeout=10)
        except subprocess.TimeoutExpired:
            nest["proc"].kill()
            nest["proc"].wait()
        release_port()

    return _as_nest_handle(nest), cleanup


def _as_nest_handle(nest):
    """Wrap a raw nest dict in the guarded-capability handle for this run's mode.

    Every nest the harness starts goes through here, so slices 2 and 3 inherit
    the guard without hunting the ~17 `_make_nest` call sites. On the default
    path this is a no-op by construction: standalone declares every capability,
    so `NestHandle` behaves exactly like the plain dict it replaces
    (`helpers/nest_mode.py`, and `test_nest_mode_axis.py` pins it).

    Same reasoning for the port→scheme fact: a handle that declares `serve_tls`
    registers its port with `common.auth.mark_tls_nest`, the ONE place that fact
    lives, so every helper keyed by *port* rather than by URL (`ws_api.nest_info`,
    `_resolve_base`, and the ~dozen `common.auth` dials behind them) speaks
    `https://`/`wss://` to a TLS listener instead of plain HTTP.

    Registering it *here* rather than in the provider is what makes it hold for
    any future TLS-serving mode — and the omission was not hypothetical: it is
    why the axis could never record a docker run. `_DockerProvider` returns
    `https://127.0.0.1:<port>` + `serve_tls: True` (the image synthesizes a
    self-signed floor cert at boot and has no plain-HTTP posture), but with the
    port unregistered `_apply_r14_trust_env`'s `ws_api.nest_info(port)` dialled
    `ws://`, the listener closed the connection, and every app-fixture test
    errored at setup before its first assertion.
    """
    from helpers import nest_mode as nest_mode_mod

    if nest.get("serve_tls"):
        from common.auth import mark_tls_nest

        port = nest.get("port")
        if port:
            mark_tls_nest(port)

    # The port→HOST half of the same fact: a live box is not on loopback, and a
    # port-keyed dial that assumed it was reached `127.0.0.1:443` instead of the
    # box (`common.auth._NEST_HOSTS`). Recorded for every nest, so a loopback
    # nest reusing a port takes the authority back from whoever held it.
    from urllib.parse import urlparse

    url_host = urlparse(nest.get("url") or "").hostname
    if url_host and nest.get("port"):
        from common.auth import mark_nest_authority

        mark_nest_authority(nest["port"], url_host)

    # `peer_url` is a CONTRACT key every mode answers — the authority ANOTHER
    # NEST in the run dials, as distinct from `url`, the authority the harness
    # and the client dial (`testing.md` § Default app and nest mode, ruling (2)).
    # They coincide wherever there is no network boundary in between (standalone
    # and live are both reached at one authority by everybody), so the default is
    # `url`; docker's provider sets its own — the container IP on the run's
    # network — before it gets here, and this must not overwrite it.
    #
    # Contract rather than capability deliberately: a mode that could DECLINE the
    # key would make "this mode has no second nest" and "this nest cannot be
    # dialled by a peer" the same absence, and they are different facts. The
    # second one is the nest's SSRF posture working (exclusion class (8)), not a
    # harness gap, so it is classified at collection and never mistaken for a
    # missing capability at runtime.
    nest.setdefault("peer_url", nest.get("url"))

    mode = nest_mode_mod.run_mode()
    return nest_mode_mod.NestHandle(
        nest, mode, absent=nest_mode_mod.absent_capabilities(mode),
    )


#: The container-name prefix the axis used before it labelled anything. The sweep
#: unions it exactly as the Hetzner sweep unions its `e2e-` prefix: without it the
#: containers a pre-label session leaked can never be reclaimed by anything but a
#: human, because they carry no owner to check.
_DOCKER_LEGACY_PREFIX = "fauna-nest-axis-"

#: Ruling (4)'s two labels. `fauna-e2e=1` marks the harness's own containers and
#: networks (the live suite's provider-label sweep, applied to docker);
#: `fauna-e2e-run=<run id>` names WHICH run owns one, which is what lets a sweep
#: distinguish its own leftovers from a sibling session's live containers.
_DOCKER_LABEL_HARNESS = "fauna-e2e"
_DOCKER_LABEL_RUN = "fauna-e2e-run"

_docker_run_id_cache = None


def _format_docker_run_id(pid, start) -> str:
    """`<pid>-<start>`, or a bare pid where the start time cannot be read.

    Kept to `[A-Za-z0-9.-]` so the same token is legal as a docker LABEL value
    and as a docker NETWORK name — the run's network is named after it, so one
    token identifies both halves of what a run must clean up.
    """
    return f"{pid}-{start}" if start not in (None, "") else str(pid)


def _docker_run_id() -> str:
    """This run's owner token: the pytest process's pid plus its start time.

    Pid alone is not enough. A leaked container can outlive its run by days on a
    shared dev box, and by then the pid may have been reused by an unrelated
    process — a sweep keyed on the bare pid would then read a genuinely dead run
    as alive and leak forever, or read a live one as dead and delete it. The
    start time is the disambiguator, exactly as the fleet's own session-liveness
    tooling disambiguates its session locks.
    """
    global _docker_run_id_cache
    if _docker_run_id_cache is None:
        _docker_run_id_cache = _format_docker_run_id(
            os.getpid(), _proc_start_ticks(os.getpid()),
        )
    return _docker_run_id_cache


def _parse_docker_run_id(run_id):
    """`(pid, start)` from a run id, or `(None, None)` if it is not one of ours."""
    pid, _, start = str(run_id).partition("-")
    if not pid.isdigit():
        return None, None
    return int(pid), (start or None)


def _proc_start_ticks(pid: int):
    """`/proc/<pid>/stat` field 22 — start time in clock ticks since boot — as a
    string, or None off-Linux and where it cannot be read.

    The comm field can contain spaces and parentheses, so the split is after the
    LAST `)`; `rest[0]` is then field 3 and field 22 is `rest[19]`.
    """
    try:
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    rest = stat.rsplit(")", 1)[-1].split()
    return rest[19] if len(rest) > 19 else None


def _pid_alive(pid: int):
    """True / False / **None where the OS will not say** — three answers, not two.

    "Dead" and "I could not ask" are different facts and only the first licenses
    deleting somebody's container, so a `PermissionError` (the process exists and
    belongs to another user) and an unexpected `OSError` must not collapse into
    False. On Windows this deliberately does NOT go through `os.kill(pid, 0)`:
    CPython maps every signal but CTRL_C/CTRL_BREAK to `TerminateProcess`, so the
    harmless POSIX idiom would kill the process it asked about — there is no
    docker-mode run on Windows today, so the honest answer there is "unknown".
    """
    if sys.platform == "win32":
        return None
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return None


def _docker_run_is_alive(run_id) -> bool:
    """Does the run that owns this container still exist?

    Pid **plus start time**: a leaked container can outlive its run by days on a
    shared dev box, and by then the pid may have been reused by an unrelated
    process. This is the same two-part test the fleet's own session-liveness
    tooling makes on its session locks, deliberately re-implemented here in about
    fifteen lines rather than imported from it — that tooling is not part of the
    published tree, so a harness that reached into it would work on a dev box and
    fail on a contributor's clone. The duplication is the cheap side of that
    trade; the semantics are pinned by this module's own tests.

    **Fails CLOSED — an unknown owner reads as ALIVE.** A malformed label is
    somebody else's convention, and a pid the OS declines to answer for is a
    question we did not get to ask; deleting a container on either basis is how a
    sweep eats a sibling session's work. The cost asymmetry is the inverse of the
    fleet probe's (which fails OPEN, because there a false "active" only starves
    a queue): here a false "dead" destroys a running test.
    """
    pid, start = _parse_docker_run_id(run_id)
    if pid is None:
        return True
    alive = _pid_alive(pid)
    if alive is None:
        return True
    if not alive:
        return False
    ticks = _proc_start_ticks(pid)
    # Only Linux answers the start time; elsewhere pid-alive is the whole of what
    # can be checked, and a start half that is not a tick count (a run id from
    # another platform) is not evidence of anything either way.
    if ticks is not None and start is not None and start.isdigit():
        return ticks == start
    return True


def _docker_run_labels() -> dict:
    """The labels every container and network this run starts carries."""
    return {_DOCKER_LABEL_HARNESS: "1", _DOCKER_LABEL_RUN: _docker_run_id()}


#: What `docker run` says when the host port is already taken. Matched on the
#: message rather than on an exit code because docker returns 125 for every
#: daemon-side refusal alike, and retrying a DIFFERENT failure — a bad image, a
#: missing network — would just spend a second container start to fail the same
#: way with a worse diagnosis.
_PORT_COLLISION_MARKERS = ("address already in use", "port is already allocated")


def _is_port_bind_collision(exc: BaseException) -> bool:
    return any(m in str(exc).lower() for m in _PORT_COLLISION_MARKERS)


def _docker_run_network() -> str:
    """This run's one user-defined network.

    One per RUN, not one per container: ruling (1)'s topology — the venue's
    proven shape, where sidecars are addressed by IP literal because docker's
    embedded resolver SERVFAILs under churn. Naming it after the run id means two
    sibling sessions on the shared dev box cannot collide on it, and a crashed
    run's network is identifiable by the same owner check its containers are.
    """
    return f"fauna-e2e-net-{_docker_run_id()}"


#: The env names `extra_env` may carry into a container. Ruling (3) admits the
#: option "restricted to catalogued IPC", and this is that restriction.
#:
#: **Why a restriction at all.** An unrestricted `extra_env` is a general-purpose
#: entrypoint-env hatch — a knob nobody catalogued, reachable in one mode only,
#: standing in for a Rust constant or an app-set choice. That is the
#: configuration-file theatre the invariant bans, and it would arrive here
#: disguised as test convenience.
#:
#: **Why these three.** `FAUNA_DEPLOYMENT_SEED` and `FAUNA_LOG_LEVEL` are
#: artifact-set IPC in `installers/docker.md` § Environment Variables, and a
#: test pinning one is asking the deployment for the same value cloud-init would
#: write. `RUST_LOG` is deliberately NOT in that catalogue and is not an
#: oversight there: it is the nest's own diagnostic level, not a deployment
#: input, which is why it is admitted here by the ratified paragraph's own words
#: ("the log level for the crash beacon") rather than by an appeal to a table it
#: does not belong in. The catalogue pin in `test_nest_mode_axis.py` checks the
#: `FAUNA_*` half for exactly that reason.
#:
#: The set is disjoint from the env the provider sets itself — a fixture must not
#: be able to move the port, the NAT-mode seed or the claim code out from under
#: the handle it is about to be given — and that disjointness is pinned, so it
#: survives a later name being added to either side.
_DOCKER_EXTRA_ENV = frozenset({
    "FAUNA_DEPLOYMENT_SEED",
    "FAUNA_LOG_LEVEL",
    "RUST_LOG",
})


def _publish_host_for(dial_host) -> str:
    """The host interface a container's ports are published on, for a nest a
    client will dial at `dial_host`.

    The exact mirror of what `common.nest.start_nest` does to a standalone
    nest's bind, and deliberately keyed on the SAME predicate rather than a
    second copy of the rule: `_is_loopback_host` is the Python twin of the
    client's own `is_loopback_authority` (the literal `localhost` and the IP
    literals only — DNS is not resolved), and it is what decides which trust
    branch the nest under test will drive. A docker-side re-implementation could
    drift by precisely those cases.

    Widening, not moving: `0.0.0.0` keeps loopback reachable, so the harness's
    own health probe and claim call go on dialling `127.0.0.1` — they are not
    the thing under test, the client's trust path is.
    """
    from common.nest import _is_loopback_host

    return "127.0.0.1" if _is_loopback_host(dial_host or "127.0.0.1") else "0.0.0.0"


class _OptionAwareProvider:
    """The `supported_options` half of the provider protocol (ruling (3)).

    Shared by the two providers that cannot honour everything, so the refusal
    they raise and the declaration the classifier reads are one fact rather than
    two that can disagree. Standalone does not inherit it: it supports
    everything, so it has no refusal to make.

    **This is the BACKSTOP, never the classifier** — ruling (3) is explicit about
    that. A fixture whose options a mode cannot meet should be excluded at
    collection with a named class (4), so the run reports a declared absence
    instead of a setup error; this raise is what catches the case the table has
    not learned yet.
    """

    supported_options: frozenset = frozenset()

    def _refuse_unsupported(self, kwargs, *, remedy: str) -> None:
        from helpers import nest_mode as nest_mode_mod

        # A FALSY option is not a request. `_make_nest`'s defaults are
        # `False`/`None`, so a fixture spelling one out explicitly is asking for
        # the default — and refusing those would exclude exactly the zero-option
        # fixtures that are the first able to run in a container.
        wanted = {k for k, v in kwargs.items() if v}
        unsupported = sorted(wanted - self.supported_options)
        if not unsupported:
            return
        have = (
            "supports no per-nest start options"
            if not self.supported_options
            else f"supports only {sorted(self.supported_options)}"
        )
        # Loud, not ignored: a fixture asking for an option and silently
        # not getting it is the class of bug the whole capability guard exists to
        # prevent — it would run against a nest that is not the one it described
        # and report a pass.
        raise nest_mode_mod.NestModeError(
            f"nest mode {self.name!r} cannot honour {unsupported}: it "
            f"{have}. {remedy}"
        )


class _StandaloneProvider:
    """Mode 1: the harness spawns the locally-built `fauna-nest` binary.

    The default, and the only provider slice 1 ships. It is deliberately a thin
    shell over the pre-existing `_make_nest` rather than a rewrite of it: the
    axis must add ZERO cost and zero behavior change to the debugging inner loop
    (`testing.md:44`), and the cheapest way to guarantee that is for standalone
    to keep running exactly the code it ran before the axis existed.
    """

    name = "standalone"
    builds_local_nest = True

    #: Ruling (3)'s seam: every provider declares which per-nest start options it
    #: honours, so the question can be answered at COLLECTION time — before a
    #: container exists — rather than by trying and failing at setup.
    #:
    #: Standalone's provider IS `_make_nest`, so this is a fact about that
    #: function's signature. Written out rather than derived from it at runtime,
    #: and then PINNED to the signature by `test_nest_mode_axis.py` — the same
    #: hand-table-plus-equality-pin shape `LOCAL_NEST_FIXTURES` uses, and for the
    #: same reason: a set that derives itself can never disagree with reality, so
    #: it can never tell you reality moved.
    supported_options = frozenset({
        "unclaimed", "claim_domain", "handle_domain_seed",
        "serve_tls", "dial_host", "extra_env", "cors_origins", "static_dir",
    })

    def start(self, nest_binary, tmp_path_factory, label="nest", **kwargs):
        return _make_nest(nest_binary, tmp_path_factory, label, **kwargs)

    #: Ruling (3)'s venue seam, standalone half. See `_start_mail_venue`.
    supported_venue_options = frozenset({"registration_open", "caldav_only",
                                         "caldav_admin_port"})

    def start_mail_venue(self, request, tmp_path_factory, label, **options):
        """Standalone's mail venue: a nest plus HOST-SPAWNED MTA/MDA bridges.

        A thin adapter over `_dedicated_mail_nest_impl` for the same reason
        `start` is a thin shell over `_make_nest` — the axis must add zero cost
        and zero behaviour change to the default inner loop, and the cheapest
        guarantee of that is running exactly the code that ran before the seam
        existed. `close()` on the generator raises `GeneratorExit` at its
        `yield`, which is what runs its `finally` (bridges, stub MX, fakes and
        nest torn down in order) — so the teardown is the same code too.
        """
        gen = _dedicated_mail_nest_impl(
            request.getfixturevalue("mail_bridge_binary"),
            request.getfixturevalue("seal_helper_binary"),
            request.getfixturevalue("nest_binary"),
            tmp_path_factory, label=label, **options,
        )
        return next(gen), gen.close


def _docker_since_now() -> str:
    """An RFC3339 UTC instant for `docker logs --since`.

    The daemon is on this machine, so its log timestamps and this clock are the
    same clock — which is what makes "everything after this instant is new"
    true rather than hopeful. Nanosecond-shaped and `Z`-suffixed because that is
    the form docker's own timestamp parser is documented against.
    """
    return (datetime.datetime.now(datetime.timezone.utc)
            .strftime("%Y-%m-%dT%H:%M:%S.%f000Z"))


class _ContainerLogStream:
    """The `log_path` capability for a containerised nest: `docker logs
    --follow` redirected into a host file.

    **This is a lifted absence, not a new affordance.** `log_path` sat in
    `DOCKER_ABSENT` until 2026-09-02 with the reason "the nest logs to the
    container's stdout under s6, read with `docker logs`, not to a file the
    harness owns" — a sentence that describes the transport and names the door.
    The table's contract is *a fact about the image, not a harness limitation to
    be lifted later*, and this entry was the second kind: the lines exist, the
    door is first-class, and `RUST_LOG` reaches the image's nest, so the
    dispatch beacon's DEBUG level arrives here as readily as it does beside a
    binary. What was missing was only a harness that opened it.

    **A file, because the file is the capability.** `NestLogWatch` records a
    byte offset before the action under test and re-reads from it, so what a
    reader needs is a path to something that grows — not a stream object, and
    not a `docker logs` invocation it would have to know to make. Publishing the
    path is what lets `helpers/crash_recovery.py`, `test_web_claim_pin_wasm_
    witness.py` and every other reader stay mode-blind.

    **stdout to the FILE, never a PIPE** (e2e-conventions.md point 13): this
    child writes for as long as the nest lives, so an undrained pipe blocks it
    the moment ~64 KB fill — and a blocked writer is indistinguishable from a
    quiet nest, which is the exact question the log is being read to answer.
    `stderr` is folded in because tracing writes there; dropping it would
    publish an empty file for a nest that was logging all along.

    **What it contains differs from standalone's, deliberately.** Standalone's
    `nest.log` is one process's output; this is the whole container's — s6, the
    sni-router, the sidecars, the bridges, and the nest. For a mode whose point
    is the deployed artifact that is the more honest answer rather than a lossy
    one, and it is why the stream does not filter: any rule narrow enough to
    keep only "the nest's" lines would eventually drop the one line a test was
    waiting for. Callers already match on beacons specific enough to be
    unambiguous (`ws-rpc dispatch received`, emitted by nest and nothing else).
    """

    def __init__(self, name: str, path: str):
        self.name = name
        self.path = path
        self._proc = None
        self._fh = None
        self.attach()

    def attach(self, since: str | None = None) -> None:
        """Start following the container's log into `path`.

        With no `since` this replays the container's whole log — which is what
        the FIRST attach wants: the boot lines are exactly what a test watching
        for a nest coming up reads, and a stream that started at "now" would
        race the container's own startup for them.

        The file mode follows from that and is not incidental. A full replay
        OWNS the file and truncates, so a start retried after a bind collision
        does not leave the dead container's boot lines above the live one's — a
        reader that scans the whole file rather than from an offset (the claim
        banner is one) would otherwise answer with the wrong box's identity. A
        re-attach APPENDS, because it is continuing the same container's log and
        a reader's byte offsets have to keep meaning what they meant.
        """
        from drivers.port_util import popen_group_kwargs

        argv = ["docker", "logs", "--follow"]
        if since is not None:
            argv += ["--since", since]
        argv.append(self.name)
        self._fh = open(self.path, "ab" if since is not None else "wb",
                        buffering=0)
        self._proc = subprocess.Popen(
            argv, stdout=self._fh, stderr=subprocess.STDOUT,
            **popen_group_kwargs())

    def reattach_since(self, since: str) -> None:
        """Follow again from `since`, after a restart (`_ContainerProc.start`)."""
        self.detach()
        self.attach(since=since)

    def detach(self) -> None:
        proc, fh = self._proc, self._fh
        self._proc = self._fh = None
        if proc is not None:
            try:
                proc.terminate()
                proc.wait(timeout=10)
            except Exception:
                with contextlib.suppress(Exception):
                    proc.kill()
        if fh is not None:
            with contextlib.suppress(Exception):
                fh.close()

    # The fixture-teardown name; the same act as `detach`, which is the name the
    # restart path reads better under.
    close = detach


class _ContainerProc:
    """The `proc` capability for a containerised nest.

    Docker's answer to standalone's `subprocess.Popen`. It implements exactly the
    surface the harness actually uses on a nest proc — `terminate`/`wait`/`kill`/
    `poll` (verified by grep across `tests/` and `conftest`) — mapped onto the
    container's own lifecycle, and deliberately nothing else: an attribute this
    does not have raises `AttributeError` naming the container, which is a far
    better diagnosis than a `Popen` look-alike quietly doing the wrong thing.

    `terminate` is `docker stop` (SIGTERM to pid 1, s6 brings the tree down
    gracefully — the same shutdown path production takes), `kill` is
    `docker kill`, and `poll`/`wait` read the container's exit state.

    `log_stream` is the container's `_ContainerLogStream`, held here for one
    reason: `start()` is the only moment that knows the container was down, and
    that instant is the boundary a re-attach needs (see `start`).
    """

    def __init__(self, name: str, log_stream=None):
        self.name = name
        self.log_stream = log_stream

    def _inspect(self, field: str) -> str:
        result = subprocess.run(
            ["docker", "inspect", "-f", field, self.name],
            capture_output=True, text=True, timeout=30,
        )
        return result.stdout.strip() if result.returncode == 0 else ""

    def poll(self):
        """`None` while running, else the container's exit code."""
        if self._inspect("{{.State.Running}}") == "true":
            return None
        code = self._inspect("{{.State.ExitCode}}")
        return int(code) if code.isdigit() else 0

    def terminate(self):
        subprocess.run(["docker", "stop", "-t", "10", self.name],
                       capture_output=True, timeout=60)

    def kill(self):
        subprocess.run(["docker", "kill", self.name],
                       capture_output=True, timeout=30)

    def wait(self, timeout: float | None = None):
        deadline = time.monotonic() + (timeout if timeout is not None else 3600)
        while time.monotonic() < deadline:
            code = self.poll()
            if code is not None:
                return code
            time.sleep(0.2)
        raise subprocess.TimeoutExpired(f"docker wait {self.name}", timeout or 0)

    def start(self):
        """Bring the SAME container back up — the second half of a benign flip.

        `docker start` on a stopped container, not `docker run` of a new one: the
        container keeps its name, its port publication and its `/data` bind-mount,
        so a client's `node_url` survives the cycle exactly as it does across
        standalone's re-spawn. What is replaced is the process tree — s6 boots the
        image's own supervision again — and with it every scrap of in-memory state,
        which is the property `restart_nest` exists to produce.

        Deliberately not modelled on Watchtower's container REPLACEMENT (stop old,
        run new from a new image on the same volume): that swaps the artifact, and
        a mode whose whole point is "this exact image" must not silently run a
        different one mid-test. The data dir, identity and claim are what carry
        across, and they do.

        The log stream is re-attached from an instant captured BEFORE the start,
        while the container is down and can therefore log nothing: `docker logs
        --follow` exits with the container, and a re-attach that replayed the
        whole history would let a beacon from an EARLIER operation satisfy a
        `NestLogWatch` armed for this one — the vacuous-kill outcome the watch
        exists to refuse. Only on a start that actually succeeded: arming that
        boundary against a container still down would leave the stream blind to
        every line of the next real restart.
        """
        boundary = _docker_since_now()
        result = subprocess.run(
            ["docker", "start", self.name],
            capture_output=True, text=True, timeout=120,
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"docker start {self.name} failed ({result.returncode}): "
                f"{result.stderr.strip() or result.stdout.strip()}"
            )
        if self.log_stream is not None:
            self.log_stream.reattach_since(boundary)


class _DockerProvider(_OptionAwareProvider):
    """Mode 2: the real deployment artifact — the nest image under s6 supervision.

    What this mode buys over standalone is the whole tier_4 delta: the Dockerfile's
    own binary, the `docker/s6/*` supervision tree, and the image's boot-time
    gates. A nest-side fix that is green here is proven in the *deployed* shape
    before anyone dispatches a production release — which is exactly the gap the
    design record names as slice 2's immediate payoff.

    **This provider never builds the image, by design.** Building the nest image
    on a dev VM is forbidden — every dev VM shares one physical host, so the
    build starves its siblings; the sanctioned path is the
    `build-nest-image.yml` dispatch on the self-hosted runner
    (`build-system.md` § Image tags & channels). So the image must already be
    present, and an absent one is a loud refusal naming both remedies rather
    than a 20-minute surprise build. `--nest docker:<ref>` picks the image
    explicitly, which is how a session runs this mode against an already-pulled
    `ghcr.io/faunasocial/nest:latest` without building anything at all.

    **`/data` is a host bind-mount**, not a named volume: that is what keeps
    `db_path`/`blob_dir`/`tmp_dir` real capabilities rather than absences (the
    design record's "worth doing"), so SQLite-poking fixtures survive the mode
    switch. It works because the image's `fauna` user is uid 1000 and so are the
    dev VMs — the host side is owned by the invoking user, not by root.
    """

    name = "docker"
    #: The binary is in the image, so a local cargo build is pure waste here.
    builds_local_nest = False

    #: Ruling (3)'s declaration: the options this mode honours, which are exactly
    #: the ARTIFACT-WIRING ones. Each is one container start parameter and each is
    #: a deployment input `installers/docker.md` § Environment Variables already
    #: catalogues — never a mounted `nest.toml` and never a new entrypoint env,
    #: both of which would be configuration-file theatre.
    #:
    #: What is deliberately absent is as load-bearing as what is here. A PRODUCT
    #: CHOICE is not a start option in any mode: it is applied after boot over the
    #: wire, the way an admin's app applies it. `registration_open` used to be the
    #: example here and is now the demonstration — arm 4 moved it to
    #: `common.auth.open_registration`, so it is not missing from this set, it no
    #: longer exists as an option at all, and every fixture that wanted an open
    #: nest stopped being a docker declared absence without this set growing.
    #: `handle_domain` split into two names in arm 4's second half, and the
    #: split is exactly what let this set grow. `claim_domain` is here because it
    #: is a WIRE ACT — the domain rides the claim this provider already makes
    #: (`claim_admin_api(mail_domain=...)`, which has carried the field since the
    #: tier_4 domained-claim test) — so honouring it costs one argument, not a
    #: mechanism. `handle_domain_seed` is deliberately absent and permanently so:
    #: it is the `--handle-domain` boot flag, and the two shapes that still need
    #: it (an IP-literal authority the claim gate refuses, an `unclaimed=True`
    #: nest with no harness claim to carry anything) cannot be expressed over the
    #: wire at all. Honouring it would mean an entrypoint env or a mounted
    #: `nest.toml`, which ruling (3) bans by name.
    #:
    #: Growing this set un-excludes every fixture needing only what it now covers,
    #: with no table edited anywhere.
    #: `cors_origins` is here because the image already carries this exact wiring:
    #: the entrypoint reads `FAUNA_CORS_ORIGINS` and seeds `[nest].cors_origins`
    #: into `/data/nest.toml` on first run (`installers/docker.md` § Environment
    #: Variables — a catalogued Seed, not a new env), so honouring it costs one
    #: variable rather than a mechanism, and the ban above stands untouched. It is
    #: needed here for the same reason as in standalone and no less: the browser
    #: runs on the host either way, so a raw-dialed containerised nest is exactly
    #: as cross-origin as a raw-dialed local one.
    #: `static_dir` is met the way `serve_tls` is, before it is asked for: the
    #: image serves its own bundled SPA build at `/app/` (and the private share
    #: link viewer page at `/share/<token>`), so a fixture asking for "a nest
    #: that serves the SPA build" gets the image's. The host path itself is not
    #: mounted — the build under test is the one the image was built from.
    supported_options = frozenset({
        "unclaimed", "claim_domain", "serve_tls", "dial_host", "extra_env",
        "cors_origins", "static_dir",
    })

    #: `handle_domain_seed` is the ONE option missing above, and its absence is
    #: permanent rather than un-built — ruling (3) settles it by name. It is the
    #: `--handle-domain` boot seed, and the only ways an image could take one are
    #: a mounted `nest.toml` or a new entrypoint env, both of which the ruling
    #: bans as configuration-file theatre. So a fixture excluded by this option
    #: has a cell that is honestly blank forever, and grading it as closable debt
    #: overstated the mode's outstanding work by the whole of this class: docker
    #: honours every OTHER option a fixture asks for today, so before this
    #: declaration `unsupported_option [MIXED]` was 100% permanent and reading as
    #: 100% closable — the last MIXED sole-blocker in the audit (`conversations`).
    permanently_unsupported_options = frozenset({"handle_domain_seed"})

    #: The default image when `--nest docker` carries no `:ref`. The tier_4
    #: suite's locally-built tag, so a session that HAS built one gets it.
    DEFAULT_IMAGE = "fauna-nest-test:local"

    def __init__(self, image: str | None = None):
        self._image = image

    def image_ref(self, mode) -> str:
        """The image this run boots, or would boot — pure, no docker call.

        `feature_ledger.image_digest_for` calls this at collection time, before
        any nest boots (`_apply_feature_axis` runs from `pytest_collection_
        modifyitems`), so it must never shell out or raise the way
        `_resolve_image` below does — a missing image is `start`'s refusal to
        report, not a collection-time failure.
        """
        return mode.argument or self._image or self.DEFAULT_IMAGE

    def _resolve_image(self, mode) -> str:
        from helpers import nest_mode as nest_mode_mod

        image = self.image_ref(mode)
        present = subprocess.run(
            ["docker", "image", "inspect", image],
            capture_output=True, timeout=60,
        )
        if present.returncode != 0:
            raise nest_mode_mod.NestModeError(
                f"nest mode 'docker' needs the image {image!r}, which is not "
                f"present on this machine. This provider deliberately does NOT "
                f"build it: building the nest image on a dev VM is forbidden — "
                f"the dev VMs share one physical host and the build starves "
                f"them (build-system.md § Image tags & channels). Either\n"
                f"  • run against an already-published image: "
                f"--nest docker:ghcr.io/faunasocial/nest:latest "
                f"(docker pull it first), or\n"
                f"  • dispatch a real build: gh workflow run build-nest-image.yml "
                f"--ref main, then pull the resulting tag.\n"
                f"Refusing rather than falling back to standalone: a silent "
                f"fallback would report a green docker run against a nest the "
                f"image never served."
            )
        return image

    #: Set once per session, the first time `start` runs: this run's network
    #: exists and the dead-run sweep has been done. Guarding on the CLASS rather
    #: than the instance keeps it one sweep per process even if the registry ever
    #: hands out a second provider object.
    _run_network_ready = False

    @classmethod
    def _sweep_dead_runs(cls) -> None:
        """Reclaim containers and networks whose owning run is gone.

        Ruling (4): convention 9 reaps every harness child with its process
        group, and a container is the one child the kernel does not reap — so a
        crashed or SIGKILLed run leaks its nests, and on a shared dev box those
        accumulate until a human notices. The first start of a session therefore
        sweeps, the way the live suite's provider does.

        Two things it deliberately does NOT do. It does not sweep by age: a
        sibling session's four-hour docker run is not garbage, and an age
        heuristic is exactly how a sweep eats live work. And it does not delete
        what it cannot account for — `_docker_run_is_alive` fails closed, so an
        unparseable owner, an unreadable pid and this run's own containers are
        all left alone.
        """
        from tests.platform.docker.helpers import remove_container, remove_network

        mine = _docker_run_id()
        for kind, args in (
            ("container", ["docker", "ps", "-a", "--filter",
                           f"label={_DOCKER_LABEL_HARNESS}=1",
                           "--format", "{{.Names}}\t{{.Label \"" + _DOCKER_LABEL_RUN + "\"}}"]),
            ("network", ["docker", "network", "ls", "--filter",
                         f"label={_DOCKER_LABEL_HARNESS}=1",
                         "--format", "{{.Name}}\t{{.Labels}}"]),
        ):
            try:
                listed = subprocess.run(args, capture_output=True, text=True, timeout=30)
            except (OSError, subprocess.SubprocessError):
                continue
            for line in listed.stdout.splitlines():
                name, _, owner = line.partition("\t")
                if kind == "network":
                    # `{{.Labels}}` is a comma-joined `k=v` list, not one value.
                    owner = next(
                        (part.split("=", 1)[1] for part in owner.split(",")
                         if part.startswith(f"{_DOCKER_LABEL_RUN}=")),
                        "",
                    )
                if not name or owner == mine or _docker_run_is_alive(owner):
                    continue
                print(f"[docker] sweeping leaked {kind} {name} "
                      f"(owning run {owner or '?'} is gone)", flush=True)
                (remove_container if kind == "container" else remove_network)(name)

        # The legacy union: `fauna-nest-axis-<port>` containers predate the
        # labels and carry no owner at all, so the label pass above cannot see
        # them. They are unambiguously this axis's and unambiguously not this
        # run's (this run's carry labels), so a name match is enough.
        try:
            legacy = subprocess.run(
                ["docker", "ps", "-a", "--filter", f"name={_DOCKER_LEGACY_PREFIX}",
                 "--filter", f"label={_DOCKER_LABEL_HARNESS}", "--format", "{{.Names}}"],
                capture_output=True, text=True, timeout=30,
            )
            labelled = set(legacy.stdout.split())
            unlabelled = subprocess.run(
                ["docker", "ps", "-a", "--filter", f"name={_DOCKER_LEGACY_PREFIX}",
                 "--format", "{{.Names}}"],
                capture_output=True, text=True, timeout=30,
            )
            for name in unlabelled.stdout.split():
                if name not in labelled:
                    print(f"[docker] sweeping pre-label leftover {name}", flush=True)
                    remove_container(name)
        except (OSError, subprocess.SubprocessError):
            pass

    @classmethod
    def _ensure_run_network(cls) -> str:
        """This run's one user-defined network, created on first use.

        Ruling (1) rejects host networking outright — the image's s6 tree binds
        fixed ports (`:443` router, `:8443` CalDAV, the mail ports), so under host
        networking two nests in one run, or two sibling sessions on this shared
        box, collide. Every container of a run joins one per-run network instead:
        the venue's proven topology, and the only shape in which a second nest can
        be addressed by a peer at all.
        """
        from tests.platform.docker.helpers import create_network

        network = _docker_run_network()
        if not cls._run_network_ready:
            cls._sweep_dead_runs()
            create_network(network, labels=_docker_run_labels())
            atexit.register(cls._remove_run_network)
            cls._run_network_ready = True
        return network

    @classmethod
    def _remove_run_network(cls) -> None:
        """Session end: this run removes what it created, by run id.

        Registered with `atexit` rather than hung off a fixture because the
        containers it holds are torn down by their own fixtures' cleanups, which
        may be finalized in any order — the network can only go once the last one
        has, and process exit is the one point that is true.
        """
        if not cls._run_network_ready:
            return
        from tests.platform.docker.helpers import remove_network

        cls._run_network_ready = False
        remove_network(_docker_run_network())

    def start(self, nest_binary, tmp_path_factory, label="nest", *,
              _venue_ports=None, _venue_env=None, _venue_dns=None, **kwargs):
        """Start a containerised nest. Returns `(NestHandle, cleanup)`.

        `nest_binary` is accepted and ignored — the binary is in the image. The
        argument stays in the signature because the provider protocol is shared
        with standalone, and a provider that silently took a different shape
        would push mode-branching back into `nest_instance`.

        **N-ary by construction** (ruling (1)): calling this again gives another
        nest — its own container, its own `/data` under pytest's tmp tree, its
        own published port — on the same run network, which is what makes a
        second or third nest the zero-option call rather than a new mechanism.

        `_venue_ports` / `_venue_env` are **this provider's own** seam, never a
        fixture's: `start_mail_venue` below needs the mail listeners published at
        container start and the scanner sidecars' addresses in the environment,
        and neither can be a per-nest start OPTION. `FIXTURE_START_OPTIONS` is
        name-level and mode-independent (ruling (3)'s seam grades option NAMES a
        fixture literally passes), so an option a fixture would need in docker
        and not in standalone is exactly what that table cannot express — which
        is why arm 6's venue is a provider METHOD. Leading underscore and
        keyword-only so they cannot arrive through `**kwargs` and be graded as
        options; pinned by `test_nest_mode_axis.py` to the one caller.
        """
        from helpers import android_venue, nest_mode as nest_mode_mod
        from common.nest import nest_id_from_data_dir
        from drivers.port_util import find_free_port
        from tests.platform.docker.helpers import (
            claim_admin_api, container_ip, generate_claim_code, remove_container,
            start_container_with_ports, wait_for_health,
        )

        self._refuse_unsupported(kwargs, remedy=(
            "Grow _DockerProvider.supported_options and honour the option as a "
            "container start parameter — never a mounted nest.toml and never a "
            "new entrypoint env, both of which would be configuration-file "
            "theatre; a product choice a user or admin makes in the app belongs "
            "after boot, over the wire, in every mode alike."
        ))

        # The artifact-wiring options, read off the same kwargs the refusal above
        # graded. `unclaimed` withholds the CLAIM, never the claim code — the
        # entrypoint writes `$FAUNA_CLAIM_CODE` to `/data/claim-code` regardless,
        # and `/data` is the handle's own `tmp_dir`, so standalone's contract
        # ("the one-time code is on disk at `<tmp_dir>/claim-code`") is answered
        # here by the image rather than by a second mechanism.
        #
        # `serve_tls` needs no honouring at all: the image serves its self-signed
        # floor cert on the listener and has no plain-HTTP posture to opt out of,
        # so the option is already met before it is asked for. It is declared
        # supported because that is TRUE, not as a shortcut — a fixture that asks
        # for TLS gets TLS.
        unclaimed = bool(kwargs.get("unclaimed"))
        claim_domain = kwargs.get("claim_domain")
        dial_host = kwargs.get("dial_host") or "127.0.0.1"
        extra_env = dict(kwargs.get("extra_env") or {})
        # `cors_origins` is honoured through the image's OWN seed path, not a
        # second mechanism: `FAUNA_CORS_ORIGINS` (comma-separated) is what the
        # entrypoint already reads to write `[nest].cors_origins` into
        # `/data/nest.toml` on first run. Standalone reaches the same config key
        # through `--cors-origin`; both are the artifact seeding a client-set
        # state, which is exactly what `registry.md` § Health-poll CORS says the
        # seed is for. Not routed through `extra_env` (and so not catalogued in
        # `_DOCKER_EXTRA_ENV`) because it is a NAMED option the classifier can
        # grade, which is the whole point of ruling (3)'s seam.
        cors_origins = list(kwargs.get("cors_origins") or ())
        # The value-level half of ruling (3)'s restriction, and it can only live
        # here: the classifier grades option NAMES — `FIXTURE_START_OPTIONS`
        # records that a fixture passes `extra_env`, never which keys — so a
        # collection-time verdict cannot be reached about the keys. Loud at setup
        # is therefore the whole of the contract, and it is a deliberate
        # exception to "the backstop is never the classifier" rather than a gap
        # in it.
        uncatalogued = sorted(set(extra_env) - _DOCKER_EXTRA_ENV)
        if uncatalogued:
            raise nest_mode_mod.NestModeError(
                f"nest mode 'docker' will not carry {uncatalogued} into a "
                f"container: `extra_env` is restricted to catalogued artifact-set "
                f"IPC ({sorted(_DOCKER_EXTRA_ENV)}) — testing.md § Default app "
                f"and nest mode, ruling (3). An uncatalogued container env is "
                f"configuration-file theatre: a knob no deployment writes, "
                f"reachable in one mode only. If the value really is deployment "
                f"wiring, catalogue it in installers/docker.md § Environment "
                f"Variables and add it here; if it is a product choice, apply it "
                f"after boot over the wire, in every mode alike."
            )

        mode = nest_mode_mod.run_mode()
        image = self._resolve_image(mode)
        network = self._ensure_run_network()
        # The nest's data dir, bind-mounted from the host so `db_path` and
        # friends stay real capabilities. `mktemp` keeps it under pytest's own
        # tmp tree, so it is cleaned on the same schedule as a standalone nest's.
        data_dir = str(tmp_path_factory.mktemp(f"{label}-data"))
        claim_code = generate_claim_code()

        # `<tmp_dir>/nest.log` — the SAME relative path standalone publishes
        # (`common.nest`), which is what lets every reader stay mode-blind. It
        # lives inside the `/data` bind mount deliberately: standalone's does
        # too, so the nest has always run beside its own log file, and the
        # factory-reset wipe removes named files only (`factory_reset.rs`), so a
        # reset does not pull the log out from under a watcher mid-journey.
        log_path = os.path.join(data_dir, "nest.log").replace("\\", "/")

        # ONE retry on a bind collision, rather than pre-reserving the port.
        # `find_free_port` binds, reads the port and closes — so between the close
        # and `docker run` a sibling session can take it (the race
        # `find_free_ports`' own docstring documents). Holding the socket open
        # instead would not help: docker cannot bind a port the harness still
        # holds. Retrying is the cheap direction, and a second collision is not a
        # race any more — it is a machine with no free ports, which must be loud.
        last_error = None
        log_stream = None
        for attempt in range(2):
            # A leased venue port in an android venue run, `find_free_port()`
            # otherwise — the same seam as the standalone provider.
            port, release_port = android_venue.nest_port(find_free_port)
            name = f"{_DOCKER_LEGACY_PREFIX}{port}"
            started = False
            try:
                start_container_with_ports(
                    name,
                    {3000: port, **(_venue_ports or {})},
                    # The provider's own wiring FIRST, so the merge order says
                    # what the refusal above already guarantees: `extra_env` adds
                    # to how this nest is reached, it never redefines it.
                    env={
                        "FAUNA_PORT": "3000",
                        "FAUNA_MODE": "public",
                        "FAUNA_CLAIM_CODE": claim_code,
                        **({"FAUNA_CORS_ORIGINS": ",".join(cors_origins)}
                           if cors_origins else {}),
                        **(_venue_env or {}),
                        **extra_env,
                    },
                    # An absolute host path here is a read-write bind-mount at
                    # /data (docker reads a `/`-bearing source as a host path, a
                    # bare name as a named volume) — which is what makes db_path,
                    # blob_dir and tmp_dir real capabilities in this mode.
                    data_volume=data_dir,
                    image=image,
                    network=network,
                    dns=_venue_dns,
                    labels=_docker_run_labels(),
                    publish_host=_publish_host_for(dial_host),
                )
                started = True
                # BEFORE the health wait, so the boot lines land in the file:
                # a startup that fails is exactly when the log is worth having,
                # and `wait_for_health`'s own failure message is the first
                # reader of it.
                log_stream = _ContainerLogStream(name, log_path)
                # Loopback, whatever the client will dial: the publication is
                # WIDENED for a LAN `dial_host`, not moved, so these two stay on
                # the interface they have always used. The client's trust path is
                # the thing under test; the harness's own is not.
                wait_for_health(port, name)
                admin = (
                    None if unclaimed
                    else claim_admin_api(port, claim_code,
                                         mail_domain=claim_domain)
                )
                break
            except BaseException as exc:
                if log_stream is not None:
                    # Before the container goes: the follower is a child of this
                    # process, and a retry that left one behind would have two
                    # writers appending to two different nests' logs.
                    log_stream.close()
                    log_stream = None
                if started:
                    remove_container(name)
                release_port()
                if attempt == 0 and _is_port_bind_collision(exc):
                    last_error = exc
                    continue
                raise
        else:  # pragma: no cover — the loop always breaks or raises
            raise last_error

        nest = {
            "proc": _ContainerProc(name, log_stream=log_stream),
            "port": port,
            # The image always serves TLS on its listener — the self-signed
            # bootstrap cert it synthesizes at boot. There is no plain-HTTP
            # docker posture to mirror standalone's default.
            #
            # The AUTHORITY is `dial_host`, because that is the whole of what the
            # option buys: a client trusting a self-signed nest short-circuits to
            # "accept" on a loopback authority and never consults the pin, so only
            # a non-loopback one can drive the SPKI-pin branch.
            "url": f"https://{dial_host}:{port}",
            # The authority ANOTHER NEST dials (ruling (2)) — the container's own
            # address on the run network, NOT the published host port: `127.0.0.1`
            # inside a container is that container. This is the only mode where
            # the two differ, which is exactly why `peer_url` had to become a
            # contract key rather than staying an alias for `url`.
            #
            # It is also, by construction, an RFC1918 address — so a nest-DIALS-
            # nest journey over it is refused by `validate_peer_url`'s
            # globally-routable requirement. That refusal is the product's SSRF
            # posture working, and it is classified as exclusion class (8) at
            # collection time; the key is still published here because the fact a
            # test needs is "this is the address a peer would use", and the fact
            # the classifier needs is "the nest will refuse it". Suppressing the
            # key would conflate them.
            "peer_url": f"https://{container_ip(name)}:3000",
            "db_path": os.path.join(data_dir, "nest.db").replace("\\", "/"),
            "blob_dir": os.path.join(data_dir, "blobs").replace("\\", "/"),
            "tmp_dir": data_dir,
            # The container's log, followed into a host file at the same
            # `<tmp_dir>/nest.log` standalone publishes — so `NestLogWatch` and
            # every other reader is mode-blind (`_ContainerLogStream`).
            "log_path": log_path,
            # The nest's own identity, read off the SAME file standalone reads
            # it from — `/data/nest_deployment.key`, which is under the host
            # bind-mount, so `common.nest.nest_id_from_data_dir` answers here
            # with no docker branch at all. Published because the federation
            # CHANNEL is harness-driven, not nest-driven: the harness dials the
            # listener while signing the `fauna.federation.hello` AS the
            # initiator, so it needs both nests' ids and the initiator's key on
            # disk. Absent it, every such test raises `KeyError('nest_id')` in
            # docker — a plain missing key rather than a declared absence, and
            # a class no classifier could name.
            "nest_id": nest_id_from_data_dir(data_dir),
            "admin": admin,
            "serve_tls": True,
            # Docker-only extras: not capabilities (every mode must answer the
            # contract keys), just what a docker-aware test needs to reach the
            # container — `docker logs`, `s6` probes, the re-claim code.
            "container_name": name,
            "claim_code": claim_code,
            "image": image,
        }

        def start_in_place():
            """Answer `common.nest.start_nest_in_place` for a containerised nest.

            The key exists because the *helper* is mode-agnostic while the act is
            not: standalone re-spawns `node_binary` against `config_path`, and both
            are declared absent here (the binary lives in the image; re-spawning a
            local one would be testing standalone while reporting docker). Rather
            than teach a shared helper to sniff for `container_name` — a per-mode
            branch inside generic code, which is the divergence priority #1 bans —
            the provider that knows how to START this nest also answers how to
            start it AGAIN.
            """
            nest["proc"].start()
            wait_for_health(port, name)

        nest["start_in_place"] = start_in_place

        def resume_after_self_exit(timeout):
            """Answer `common.nest.resume_after_self_exit` for a container.

            **Nothing to do, and that is the whole content of the answer.** A
            nest taking the production factory-reset path stages its marker and
            `exit(0)`s, expecting a supervisor to restart it into the boot-time
            wipe. Standalone has none, so the harness plays the part; here s6
            *is* that supervisor, doing in the image exactly what production
            does. A harness that also waited would be waiting on `proc.wait()`
            — the CONTAINER's exit, which never comes, because the container is
            precisely what survives the nest restarting inside it.

            What is NOT lost is the assertion. Standalone's `proc.wait()`
            witnesses "the nest exited"; the caller's own fresh/unclaimed poll
            witnesses "it came back AND it came back wiped", which is strictly
            more, and it is the same line of the same test in both modes.
            """
            del timeout  # the wait belongs to the caller's own convergence poll

        nest["resume_after_self_exit"] = resume_after_self_exit

        def cleanup():
            # The follower first: it is a child of this process holding an open
            # descriptor on the log, and `remove_container` is what makes it
            # exit anyway. Stopping it explicitly keeps teardown ordered rather
            # than racing the container's removal.
            if log_stream is not None:
                log_stream.close()
            remove_container(name)
            release_port()

        return _as_nest_handle(nest), cleanup

    #: Ruling (3)'s venue seam, docker half — the venue OPTIONS this mode
    #: honours, a separate vocabulary from `supported_options` because a venue is
    #: a provider METHOD rather than a start option (see `_start_mail_venue`).
    #:
    #: `caldav_only` is the SECOND bring-up path, and it is honoured now: with
    #: mail off the image's MDA gates on `/data/caldav-enabled` instead — its
    #: run-script re-downs itself only when NONE of
    #: `/data/{imap,caldav,carddav,webdav}-enabled` exists — so that venue drives
    #: `set_caldav_enabled` at setup and converges on a different flag and a
    #: CalDAV-only listener set, while the MTA stays down by its own gate
    #: (`/data/imap-enabled`, which a CalDAV-only deployment never writes).
    #: `caldav_admin_port` is absent PERMANENTLY, by ratification: s6 owns the
    #: rebind in the image, which is why the admin-CalDAV-port rebind test stays
    #: standalone-only (`testing.md` § Default app and nest mode, ruling (3)).
    supported_venue_options = frozenset({"registration_open", "caldav_only"})

    def start_mail_venue(self, request, tmp_path_factory, label, **options):
        """Docker's mail venue: the image's OWN s6 bridges, ports published.

        Ruling (3): *a host-spawned bridge is not a knob* — the docker shape of a
        mail nest is the image's own supervised bridges enabled through the
        product's toggle, its mail ports published to free host ports, the
        venue's sidecars beside it. So this is one container, not a container
        plus two host processes, and the whole of the difference is where the
        bridges live.

        The container is started through this provider's own `start`, so the
        venue inherits the run network, the labels and dead-run sweep (ruling
        (4)), the bind-collision retry, the health wait and the claim — a venue
        that hand-rolled `docker run` would be exactly the leak that ruling's
        label sweep exists to prevent.
        """
        from clients.ws_rpc_admin_client import WsRpcAdminClient
        from drivers.port_util import find_free_ports
        from helpers import nest_mode as nest_mode_mod
        from tests.platform.docker.helpers import (
            container_ip, remove_container, start_fake_dns_sidecar,
            start_fake_scanner_sidecars, write_dns_records,
        )

        unsupported = sorted(set(options) - self.supported_venue_options)
        if unsupported:
            raise nest_mode_mod.NestModeError(
                f"nest mode 'docker' cannot honour the mail-venue options "
                f"{unsupported}: it supports only "
                f"{sorted(self.supported_venue_options)}. This is the venue "
                f"half of ruling (3)'s seam and it is a BACKSTOP, never the "
                f"classifier — a fixture whose venue options this mode cannot "
                f"meet belongs in helpers/nest_surface.py's "
                f"MAIL_VENUE_FIXTURE_OPTIONS so the run reports a declared "
                f"absence at collection instead of exploding at setup."
            )

        # **The CalDAV-only venue is the MTA-free shape of this method**, and
        # every difference below follows from that ONE fact rather than from a
        # second setup path: it publishes no SMTP/IMAP port (the image's MTA
        # gates on `/data/imap-enabled`, which this deployment never writes, and
        # the MDA binds each DAV protocol per its own `fetch_config` flag), it
        # needs neither content scanners nor a resolver (both exist for the
        # inbound SMTP perimeter — a venue that terminates no SMTP meets none of
        # those gates), and it converges at SETUP because it drives its own
        # enable rather than waiting for one the app under test fires.
        caldav_only = bool(options.get("caldav_only"))

        # The scanners must exist BEFORE the nest container, because their
        # addresses are container env. They are IP literals rather than names:
        # docker's embedded resolver SERVFAILs under churn (`container_ip`'s own
        # docstring), and the fail-closed inbound scan gate is exactly where that
        # would surface as a mystery 451.
        network = self._ensure_run_network()
        fakes_dir = str(_repo_root / "tests" / "e2e-unified" / "fakes")
        suffix = find_free_ports(1)[0]
        sidecars: list[str] = []
        scan: dict = {}
        if not caldav_only:
            clamd_name = f"{_DOCKER_LEGACY_PREFIX}clamd-{suffix}"
            rspamd_name = f"{_DOCKER_LEGACY_PREFIX}rspamd-{suffix}"
            scan = start_fake_scanner_sidecars(
                network, fakes_dir, clamd_name=clamd_name,
                rspamd_name=rspamd_name)
            sidecars += [clamd_name, rspamd_name]

        # **A published container port turns ON every loopback-exempt perimeter
        # gate at once, and that — not the MX override — is why this venue needs
        # a resolver.** The MTA exempts loopback peers from the HELO-identity and
        # sender-domain checks by design (`internal/mta/server.go`: "local relays
        # / cron legitimately send from non-resolving sender domains", mirroring
        # postfix's permit_mynetworks ordering). A standalone venue is a host
        # process its tests dial at 127.0.0.1, so it never meets either gate; a
        # container sees the same client arrive over the published port as the
        # docker gateway, so it meets ALL of them. They surfaced one at a time,
        # each as a plausible-looking one-off — `554 HELO identity check failed`,
        # then `550 Sender domain has no DNS records` — which is the shape worth
        # remembering: the venue difference is the CLASS, not the individual gate.
        #
        # The sender-domain gate is answered by making it PASS rather than by
        # switching it off, which is the higher-fidelity of the two moves and the
        # one ruling (3) already names ("the venue's sidecars (fake DNS,
        # scanners, stub MX) beside it"). Records are published AFTER the
        # container starts, through the bind-mounted `records_dir`, because the
        # A record's value is the container's own IP — the chicken-and-egg
        # `write_dns_records` exists for.
        def _drop_sidecars():
            for sidecar in sidecars:
                remove_container(sidecar)

        dns_ip = None
        records_dir = None
        if not caldav_only:
            dns_name = f"{_DOCKER_LEGACY_PREFIX}dns-{suffix}"
            records_dir = str(tmp_path_factory.mktemp(f"{label}-dns"))
            try:
                dns_ip = start_fake_dns_sidecar(
                    network, fakes_dir, name=dns_name, txt_records={},
                    records_dir=records_dir)
            except BaseException:
                _drop_sidecars()
                raise
            sidecars.append(dns_name)

        try:
            # One allocation for the whole publication: `find_free_ports(n)`
            # exists for exactly this N-at-once case, and its close-then-reuse
            # race is why `start` retries a bind collision rather than
            # pre-reserving. The CalDAV-only venue publishes ONE port, and that
            # is what `DockerMailVenueHandle` reads its shape from.
            venue_container_ports = (8443,) if caldav_only else (25, 465, 587, 993, 8443)
            mail_ports = dict(zip(
                venue_container_ports, find_free_ports(len(venue_container_ports))))
            nest, nest_cleanup = self.start(
                None, tmp_path_factory, label,
                _venue_ports=mail_ports,
                _venue_env={
                    # Absent on the CalDAV-only venue: the entrypoint writes
                    # these into the operator hatch for the MTA's inbound
                    # content scan, and a deployment with no MTA terminates no
                    # inbound SMTP to scan.
                    **({} if caldav_only else {
                        "FAUNA_CLAMD_ADDR": scan["clamd_addr"],
                        "FAUNA_RSPAMD_URL": scan["rspamd_url"],
                    }),
                    # Publishing 8443 is necessary and NOT sufficient, and this
                    # is the measurement that says so (2026-09-02): without
                    # this, the entrypoint pins CalDAV to the container's OWN
                    # loopback at `127.0.0.1:8444`, behind the always-up
                    # `fauna-sni-router` which fronts :443 and routes DAV only
                    # by a `mail.<domain>` SNI. A published 8443 then reaches
                    # nothing that will talk TLS, and the symptom is the least
                    # informative one there is — a TCP accept followed
                    # immediately by EOF, for sixty seconds, while every mail
                    # listener beside it serves perfectly.
                    #
                    # `FAUNA_LAN_BIND_IP` is the entrypoint's own answer: it
                    # writes the `caldav_bind_host` hatch and the MDA binds that
                    # interface directly, router-bypassing, with the admin port
                    # still governing the number. Catalogued Artifact-set IPC
                    # (`installers/docker.md` § Environment Variables), so it is
                    # deployment wiring rather than a knob — the same value and
                    # the same reason `test_caldav_bare_ip_serving.py` uses:
                    # a bridge-network harness cannot predict the container's
                    # eth0 IP before start, so it binds all interfaces to make
                    # the published port reachable, exercising the identical
                    # entrypoint + MDA path a real home box takes.
                    "FAUNA_LAN_BIND_IP": "0.0.0.0",
                },
                _venue_dns=dns_ip,
            )
        except BaseException:
            _drop_sidecars()
            raise

        try:
            domain = MAIL_PRIMARY_DOMAIN
            # **The domain set is the standalone hatch's, one for one** — this
            # is where "the MX override moves from the operator-hatch to DNS"
            # actually lands. `_dedicated_mail_nest_impl` writes an
            # `mta_mx_override` for `external.test`, `example.com` and the
            # primary; the image's bridges read their own config and never see a
            # hatch, so the same three domains have to become resolvable the
            # ordinary way. Measured: the venue's first DNS cut published only
            # the primary and the inbound leg still failed, because the sender a
            # normal-MUA round-trip uses is `sender@external.test` — the gate
            # asks about the ENVELOPE sender's domain, which on an inbound
            # delivery is never the local one.
            #
            # Existence is all that is published, and deliberately: the gate asks
            # only whether the domain resolves (MX or A). A passing SPF /
            # `p=reject` DMARC pair is what `publish_passing_mail_dns` adds for
            # the ENFORCED perimeter, and publishing it here would tighten a
            # venue whose whole posture is the relaxed one — turning gates a
            # consuming test currently passes into gates it would have to satisfy
            # against a gateway IP it cannot predict. Nor do these records make
            # outbound DELIVERY to those domains meaningful: that is the stub MX's
            # job, and every test that reads one is a per-test declared absence in
            # this mode (`MAIL_VENUE_HOST_AFFORDANCE_READERS`).
            #
            # Skipped entirely on the CalDAV-only venue, which runs no resolver
            # for the same reason it runs no scanners: both gates this answers
            # (HELO identity, envelope-sender domain) are the inbound SMTP
            # perimeter's, and this deployment terminates no SMTP.
            if not caldav_only:
                _resolvable = [domain, "external.test", "example.com"]
                _nest_ip = container_ip(nest["container_name"])
                write_dns_records(
                    records_dir,
                    a={d: _nest_ip for d in _resolvable},
                    mx={d: f"0 {d}" for d in _resolvable},
                )
            admin = nest["admin"]
            admin_ws = WsRpcAdminClient(
                nest["url"],
                actor_id=bytes(admin["signing_key"].verify_key),
                signing_key=bytes(admin["signing_key"]),
            )
            # The SAME two wire calls the standalone venue makes, with the same
            # arguments, so the venue contract is one fact rather than two that
            # can drift. (`mta_sts_cert_mode` differs from the tier_4 suite's own
            # `self_signed`, and that is inert: this row measured that the field
            # is stored and echoed but no reader in `dns_handlers.rs` or
            # `dns_verifier.rs` derives anything from it.)
            with admin_ws:
                admin_ws.call(
                    "fauna.bridges.add_local_domain",
                    {"domain": domain,
                     "mta_sts_cert_mode": "per_host"},
                )
                admin_ws.call(
                    "fauna.bridges.put_spam_policy",
                    {
                        "dnsbl_servers": [],
                        "greylist_enabled": False,
                        "greylist_delay_secs": 0,
                        "fcrdns_mode": "off",
                        "max_conn_per_min": 1000,
                        "baseline_standing_publish": False,
                        # The ONE policy field that differs from standalone's
                        # call, and it is a venue fact rather than a weakening.
                        # The HELO-identity gate is loopback-EXEMPT, so a
                        # standalone venue never meets it: its MTA is a host
                        # process and the test's SMTP client dials 127.0.0.1. A
                        # container's MTA sees the same client arrive over the
                        # published port as the docker gateway — a non-loopback
                        # peer — so the gate fires, and it can only ever fail:
                        # the EHLO host is this deployment's own mail domain,
                        # which has no A record pointing at the gateway and is
                        # not going to grow one in a hermetic run. Measured
                        # 2026-09-02 as `554 5.7.0 HELO identity check failed`
                        # on the first real docker venue run. Setting it false
                        # here REPRODUCES standalone's effective posture rather
                        # than relaxing past it; `relax_spam_policy` already
                        # documents the identical move for the cross-container
                        # MTA→MTA relay, which meets the gate the same way.
                        "helo_identity_required": False,
                    },
                )
                if caldav_only:
                    # Explicit-off BEFORE the CalDAV enable, in the same order
                    # standalone uses and for the same reason it gives: an unset
                    # `mail_enabled` already reads OFF post-Stage-5, so this is
                    # redundancy that states the deployment's intent — and it
                    # pins the CalDAV toggle's independence from mail. Order is
                    # load-bearing rather than cosmetic: each handler reconciles
                    # the supervisor from the whole toggle tuple
                    # (`bridge_blob_handlers.rs::set_caldav_enabled_handler` —
                    # "the MDA is up iff mail || caldav || carddav || webdav"),
                    # so mail-off first leaves the MDA down and the CalDAV enable
                    # is then the single act that brings it up.
                    admin_ws.call("fauna.bridges.set_mail_enabled",
                                  {"enabled": False})
                # CalDAV: the product's own door for what standalone pins with an
                # operator hatch. `_spawn_mda_bridge(pin_caldav_hatch=True)`
                # writes `caldav_listen_https` into the hatch file, so the
                # standalone MDA binds a CalDAV listener whatever the deployment
                # toggle says — which is why every consumer of this venue reads
                # `caldav_port` after a MAIL enable and never enables CalDAV at
                # all. The image has no hatch and gates the four DAV protocols
                # INDEPENDENTLY (`docker/s6/fauna-mail-bridge-mda/run`: the one
                # MDA process hosts all four and binds each per its own
                # `fetch_config` flag), so mail-enable alone leaves 8443 unbound
                # — measured as a 120s `SSLError` against a container whose IMAPS
                # and SMTP listeners were all serving. Enabling it here is the
                # faithful translation: the venue promises mail + CalDAV, and in
                # this mode the way to have CalDAV is to turn it on. It also
                # brings the MDA up EARLIER, which is closer to standalone (whose
                # MDA is spawned at fixture setup) rather than further; the MTA
                # is unaffected, gating on `/data/imap-enabled` alone.
                admin_ws.call("fauna.bridges.set_caldav_enabled",
                              {"enabled": True})
            if options.get("registration_open"):
                from common.auth import open_registration
                open_registration(nest)

            handle = DockerMailVenueHandle(
                nest=nest, domain=domain, mail_ports=mail_ports,
                container_name=nest["container_name"],
            )
            if caldav_only:
                # **This venue converges at SETUP, and the mail one does not** —
                # the difference is who fires the enable. A mail venue's test
                # drives it through the app's own mail-settings page and then
                # asks `rebind_after_enable()`; a CalDAV-only deployment has no
                # such client step (its consumers only mint per-actor mailboxes),
                # so the venue that turned CalDAV on is the one that must wait.
                # Standalone hands back an already-serving MDA, so anything less
                # here would be a different contract wearing the same name.
                #
                # Generous and latency-independent (convention 14): a cold
                # container has to reach keypairs, auto-approval, the
                # restart-as-approved cold boot and the cert fan-out before the
                # listener accepts, and every step polls an observable.
                handle.rebind_after_enable(mta=False, timeout=180.0)
        except BaseException:
            nest_cleanup()
            _drop_sidecars()
            raise

        def cleanup():
            nest_cleanup()
            _drop_sidecars()

        return handle, cleanup


class _LiveProvider(_OptionAwareProvider):
    """Mode 3: an already-deployed real box (default example.com).

    What this mode buys over the other two is everything a fresh private nest
    cannot have: real DNS, a real CA-issued cert, a real network path, and —
    the one that catches bugs — **accumulated state**. It is the only mode that
    fails a works-on-a-fresh-box-only bug.

    **It starts nothing.** There is no provisioning step, so `start` is a
    resolve-and-verify: point at the box, prove it is actually up, and hand back
    a handle that declares every local-machine capability absent
    (`nest_mode.LIVE_ABSENT`). That absence is the whole safety mechanism — a
    fixture reaching for `db_path` or `proc` against a shared production box gets
    a self-diagnosing refusal naming the marker to add, not a `KeyError` and not
    a `None` that lets it skip its own assertion.

    **Admin comes from the machine's ambient identity store, never a path in
    code** (`testing.md` § Default app and nest mode), resolved per box by
    `helpers/multiseat_config.resolve_secret(url)`: `FAUNA_LIVE_SECRET_HEX` >
    the box's own staging-box provisioning identity
    (`~/.config/fauna/staging-box/<host>.json`) > `~/.fauna-id`. That identity is the box's admin,
    so `register_user` and the whole `test_user`/`logged_in_app` chain work here
    unchanged; what makes that safe is `helpers/live_accounts.py`, which reaps
    every account the run provisions.

    **Nothing here is torn down except the run's own accounts.** No reset, no
    restart, no credential rotation — the box belongs to whoever is using it
    (§ The shared-box rule), and the machine-wide `live_box` flock serializes
    this run against sibling sessions' live runs.
    """

    name = "live"
    #: The nest is on another machine; a local cargo build serves nothing.
    builds_local_nest = False

    #: Empty, and — unlike docker's — permanently so. A start option is a
    #: property of a nest the harness STARTS, and this one was deployed by
    #: somebody else long before the run began.
    supported_options = frozenset()

    #: ...which is what this says in the vocabulary the audit grades on. The
    #: sentinel rather than a list, because the permanence is a property of the
    #: MODE (the harness did not start this nest and cannot restart it with
    #: different flags), so enumerating today's option names would quietly
    #: re-open the question every time a new one is added. Spelled as the literal
    #: because this module imports `nest_mode` lazily, inside functions, on
    #: purpose; `test_nest_mode_axis.py` pins it to `nest_mode.ALL_OPTIONS` so
    #: the two cannot drift apart.
    permanently_unsupported_options = "*"

    #: How long the box has to answer its first health probe. Generous: this is
    #: a real network to a real box, and a tight bound here would turn a slow
    #: link into a failure that reads like an outage (convention 14 — the green
    #: path pays only the real latency).
    REACHABLE_TIMEOUT_S = 60.0

    def _resolve_url(self, mode) -> str:
        """`--nest live:URL` > `FAUNA_LIVE_NEST_URL` > the shared box.

        Same precedence as every other harness input (flag beats env beats
        default), and the same default the live suites already use, so pointing
        a live run somewhere else is one flag rather than an export. The one
        implementation is `nest_mode.live_url`, which the `live_box` flock and
        the `--live-box` declaration read too.
        """
        from helpers import nest_mode as nest_mode_mod

        return nest_mode_mod.live_url(mode)

    def _admin(self, url: str):
        from helpers import multiseat_config as cfg
        from helpers import nest_mode as nest_mode_mod

        secret, source = cfg.resolve_secret(url)
        if not secret:
            raise nest_mode_mod.NestModeError(
                f"nest mode 'live' needs the admin identity of {url}, and this "
                "machine has none. The seed is read from the ambient identity "
                "store — FAUNA_LIVE_SECRET_HEX, else the box's own "
                "`~/.config/fauna/staging-box/<host>.json` (written by its "
                "provisioning test), else `~/.fauna-id` (32-byte ed25519 seed, "
                "64 hex chars) — never a path baked into the harness. "
                "Refusing rather than running unauthenticated: half a live run "
                "is worse than none."
            )
        from nacl.signing import SigningKey
        from common.auth import mint_token_via_handshake
        from helpers.live_box_door import admin_probe_what, reaching_the_live_box

        sk = SigningKey(bytes.fromhex(secret.strip()))
        # A bearer for the handful of fixtures that still want one. Minting it
        # here also DOUBLES AS THE REACHABILITY PROBE: it is a real signed
        # round trip to the box, so a wrong URL, a dead box or a wrong seed
        # fails right here with that diagnosis — naming the seed's source —
        # instead of surfacing later as an unrelated-looking test failure. A
        # box that never answers is environment, not a record: the session
        # nest is cached, so every test behind it would otherwise write a
        # setup `error` (`helpers/live_box_door.py`).
        with reaching_the_live_box(admin_probe_what(url, source), admin_probe=True):
            token = mint_token_via_handshake(url, sk)
        return {
            "signing_key": sk,
            "actor_id_hex": bytes(sk.verify_key).hex(),
            "actor_id_bytes": bytes(sk.verify_key),
            "token": token,
            # The deployment seed is handed out at CLAIM time only, and this
            # box was claimed long ago by a human. Absent, not None-by-accident.
            "deployment_seed": None,
        }

    def start(self, nest_binary, tmp_path_factory, label="nest", **kwargs):
        """Resolve the live box. Returns `(NestHandle, cleanup)`.

        `nest_binary` is accepted and ignored (it is `None` in this mode — see
        `nest_binary`'s own gate); the argument stays for the shared provider
        protocol, exactly as in `_DockerProvider`.
        """
        from urllib.parse import urlparse

        from helpers import live_accounts
        from helpers import nest_mode as nest_mode_mod

        self._refuse_unsupported(kwargs, remedy=(
            "This set stays empty: a start option is a property of a nest the "
            "harness STARTS, and this one was deployed by somebody else long "
            "before the run began. Mark the test @pytest.mark.standalone_only."
        ))

        mode = nest_mode_mod.run_mode()
        url = self._resolve_url(mode)
        parsed = urlparse(url)
        if parsed.scheme != "https":
            raise nest_mode_mod.NestModeError(
                f"nest mode 'live' refuses {url!r}: a live box is reached over "
                f"real TLS with a real certificate. A plain-http 'live' target "
                f"is either a mistake or a standalone nest wearing the wrong "
                f"mode — use --nest standalone for that."
            )
        port = parsed.port or 443
        print(
            f"[live] targeting {url} (nothing is provisioned, reset or "
            f"restarted here; accounts this run creates are reaped at teardown "
            f"— helpers/live_accounts.py)",
            flush=True,
        )
        # `_admin` wraps its signed probe in the live box door itself, where
        # it knows which seed source it is offering.
        admin = self._admin(url)

        nest = {
            "port": port,
            "url": url,
            "admin": admin,
            "serve_tls": True,
            # Live-only extra: not a capability (every mode answers the contract
            # keys), just what a live-aware test needs to name the box it hit.
            "live_url": url,
        }

        # Every handle is the SAME box and the ledger is run-wide, so only the
        # LAST handle to close reaps. A module-scoped dedicated nest's teardown
        # reaping the ledger suspended the session `test_user` mid-run
        # (measured 2026-10-05 on dev.example.com, after `test_archive_import`).
        self._open_handles = getattr(self, "_open_handles", 0) + 1
        closed = False

        def cleanup():
            nonlocal closed
            if closed:
                return
            closed = True
            self._open_handles -= 1
            if self._open_handles == 0:
                live_accounts.reap(url, admin["signing_key"])

        return _as_nest_handle(nest), cleanup

    #: A dedicated mail venue is a FRESH nest with its own claimed domain and its
    #: own bridges. Live owns neither, for the same reason it honours no start
    #: option at all: the box is a real deployment nobody's test may reshape.
    supported_venue_options = frozenset()

    def start_mail_venue(self, request, tmp_path_factory, label, **options):
        from helpers import nest_mode as nest_mode_mod

        raise nest_mode_mod.NestModeError(
            "nest mode 'live' cannot stand up a dedicated mail venue: it would "
            "mean claiming a fresh nest, registering a primary mail domain and "
            "relaxing the spam perimeter on a REAL deployment. A mail venue is "
            "a nest the harness owns; live owns none."
        )


def _register_nest_providers():
    from helpers import nest_mode as nest_mode_mod

    nest_mode_mod.register_provider(nest_mode_mod.STANDALONE, _StandaloneProvider())
    nest_mode_mod.register_provider(nest_mode_mod.DOCKER, _DockerProvider())
    nest_mode_mod.register_provider(nest_mode_mod.LIVE, _LiveProvider())


_register_nest_providers()


def _make_user(nest):
    """Register a user on a nest and create a default feed. Returns user dict."""
    # Register over WS-RPC (fauna.admin.users.create) — the HTTP /admin/api/users
    # twin was ripped, so the admin SIGNING KEY (not the bearer
    # token) is what registers a user now (common.auth.register_user).
    from helpers.live_box_door import reaching_the_live_box

    # On live, a box that stopped answering is environment, not a record
    # (`helpers/live_box_door.py`); a transparent no-op in the other modes.
    with reaching_the_live_box("registering a test user"):
        user = create_actor_and_register(
            nest["port"],
            base_url=nest["url"],
            admin_signing_key=nest["admin"]["signing_key"],
        )
    # Best-effort default "General" feed over the WS-RPC feed surface (the
    # REST twin was deleted in the WS-RPC migration). Swallow failures so a
    # feed hiccup never blocks user registration.
    try:
        from actions.api_actor import ApiActor
        ApiActor(
            nest["url"], user["token"], user["actor_id_hex"],
            bytes(user["signing_key"]),
        ).create_feed("General")
    except Exception:
        pass
    # On live this account is a real, persisting row on someone's real box; the
    # WS-RPC wire core noted it for the teardown reap when its
    # `fauna.admin.users.create` succeeded (`helpers/live_accounts.py`), as it
    # does for every account any helper or test body creates.
    return user


@pytest.fixture(scope="session")
def nest_mode(request):
    """This run's nest mode — `--nest`, else `E2E_NEST`, else standalone.

    Depending on this fixture is what puts a test on the nest axis: its id gains
    the mode stamp (`pytest_generate_tests`), and its outcome is understood to
    depend on which nest filled the slot. `nest_instance` and `_driver_cache`
    both depend on it, which covers every test that reaches a nest — including
    the plain-`app` ones, whose nest is resolved lazily and so never appears in
    their declared closure.
    """
    from helpers import nest_mode as nest_mode_mod

    # `request.param` when stamped by `pytest_generate_tests`; the module's run
    # mode otherwise (a fixture reached outside collection-time parametrization).
    return getattr(request, "param", None) or nest_mode_mod.run_mode()


@pytest.fixture(scope="session")
def nest_instance(request, nest_mode, tmp_path_factory):
    """Start a fauna-nest instance, shared across all tests.

    Resolves through the nest-mode provider registry, so `--nest docker`/`live`
    replace what fills this slot without any test changing (`testing.md`
    § Default app and nest mode). Slice 1 ships the standalone provider only;
    the others refuse the run rather than silently falling back.

    Under the opt-in ``--reclaim-cycle`` flag, the shared nest is put through ONE
    real ``factory_reset → re-claim (same identity)`` cycle right here — after the
    initial claim, BEFORE the fixture yields and therefore before any test or
    downstream fixture (``_session_primary_mail_domain``, ``test_user``,
    ``mail_bridge_mta/mda``, ``logged_in_app``, ``admin_app``) materialises. So the
    *entire* session builds against a nest that has already been wiped + re-claimed,
    and every suite that rides the shared nest exercises **post-reclaim**
    provisioning — surfacing the "works-on-first-claim, breaks-after-re-claim" bug
    class broadly (§ A; the broad sibling of the bespoke
    ``test_factory_reset_calendar_reclaim.py``).

    Resetting the shared nest *here* (at creation, before anything observes it) is
    deliberately NOT a mid-session reset of a nest other tests depend on: no test or
    fixture ever sees a pre-reset ``nest_instance``. It is the single-fixture-tree
    shape — any *separate* reclaimed nest would have to fork the whole
    ``test_user`` / ``mail_bridge_*`` / primary-domain subtree that hardcodes this
    fixture (priority #1: minimise divergence). The reset is a no-op without the flag,
    so the default path every other session shares is unchanged.
    """
    # The same seam every dedicated nest goes through — one lazy resolution of
    # the binary, not one per fixture. `_prebuild_binaries` keeps the standalone
    # build at collection time where it belongs.
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest"
    )
    if request.config.getoption("--reclaim-cycle"):
        from helpers import nest_mode as nest_mode_mod

        # The reset is `fauna.admin.factory_reset` against the session nest —
        # on live, the real box and every user on it. This refusal is what
        # lets `nest_surface.LIVE_SELF_GATED_FIXTURES` carry this fixture.
        if nest_mode_mod.run_mode().is_live:
            raise pytest.UsageError(
                "--reclaim-cycle factory-resets the session nest; never on --nest live"
            )
        from common.nest import factory_reset_and_restart
        factory_reset_and_restart(nest)  # mutates nest in place (new proc, re-claimed admin)
        nest["reclaim_cycled"] = True
    # The session nest's admin is shared from the moment it exists, not from the
    # moment `test_user` happens to be requested: a fixture that runs first (the
    # session `mail_bridge_mta`, say) must meet the shared-identity fences too.
    from helpers import shared_identity

    shared_identity.remember_shared_actor((nest.get("admin") or {}).get("actor_id_hex"))
    yield nest
    cleanup()


@pytest.fixture
def reclaimable_nest(request, nest_mode, tmp_path_factory):
    """A function-scoped, freshly-claimed dedicated nest a test may factory-reset
    + re-claim via `common.nest.factory_reset_and_restart` WITHOUT disturbing the
    session-scoped `nest_instance` other tests share.

    The home for re-claim-cycle coverage: exercising the
    `factory_reset → re-claim (same identity) → enable-mail` path locally so the
    "works on first claim, breaks after re-claim" bug class goes red in CI rather
    than only on the slow shared live box."""
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "reclaim-nest")
    yield nest
    cleanup()


@pytest.fixture
def share_viewer_nest(request, nest_mode, tmp_path_factory, static_dir):
    """A function-scoped nest serving the SPA build (``static_dir``), so its
    ``GET /share/<token>`` navigation arm answers the private share link
    viewer page (``share-links.md`` § The private-file extension). The session
    ``nest_instance`` serves no build — web tests reach the app through
    ``_serve_spa_proxy`` — while the viewer is served by the nest itself, on
    the link's own origin. Requires the web build, like ``static_dir``."""
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "share-viewer-nest", static_dir=static_dir
    )
    yield nest
    cleanup()


_IS_WINDOWS = platform.system() == "Windows"


def _apply_bridge_ffi_env(env: dict) -> None:
    """Point a spawned mail-bridge / seal-helper at its libfauna_ffi.

    Unix: the FFI lib is the flavor-private slot `just mail-bridge-ffi` owns
    (justfile's `mail-ffi-slot`, always the `release` profile here — the e2e
    harness never builds `dist`); set `LD_LIBRARY_PATH` defensively (the binary
    carries an rpath, but some shells strip RUNPATH). Windows: `just
    windows-mail-bridge-build` stages the gnullvm `fauna_ffi.dll` *beside* the
    `.exe` in `target/`, and the Windows loader searches the executable's own
    directory first — so no env wiring is needed (and `LD_LIBRARY_PATH` is
    meaningless there). Mutates `env` in place.
    """
    if _IS_WINDOWS:
        return
    cargo_release = _mail_bridge_ffi_dir()
    existing_ld = env.get("LD_LIBRARY_PATH", "")
    env["LD_LIBRARY_PATH"] = (
        f"{cargo_release}:{existing_ld}" if existing_ld else cargo_release
    )


def _mail_bridge_ffi_dir() -> str:
    """Resolve the flavor-private directory holding the bridge's libfauna_ffi.

    NOT `<cargo target>/release`, which this returned until 2026-08-29: that slot
    is written by every host build of fauna-ffi in the workspace (mac's
    `apple-ffi-host release`, any `cargo build --release -p fauna-ffi`), and
    `LD_LIBRARY_PATH` takes precedence over the binary's RUNPATH — so pointing it
    at the shared slot would load a foreign *feature flavor* into the bridge at
    run time even though the justfile links it against the labeler build. The
    slot's relative path is spelled once, in the justfile
    .

    The mail-bridge binary's CGo build pins `-Wl,-rpath,` at this same directory,
    so in principle the dynamic linker finds the .so without help; we set
    `LD_LIBRARY_PATH` defensively for shells that strip RUNPATH (some container
    images do).
    """
    target_dir = os.environ.get("CARGO_TARGET_DIR")
    if not target_dir:
        result = subprocess.run(
            ["cargo", "metadata", "--format-version=1", "--no-deps"],
            cwd=_repo_root, capture_output=True, text=True, check=True,
        )
        meta = json.loads(result.stdout)
        target_dir = meta["target_directory"]
    slot = subprocess.run(
        ["just", "mail-ffi-slot"],
        cwd=_repo_root, capture_output=True, text=True, check=True,
    ).stdout.strip()
    return os.path.join(target_dir, *slot.split("/"))


def _ensure_mail_bridge_built() -> str:
    """fauna-mail-bridge via its just recipe; returns the binary path.

    The recipe's `mail-bridge-ffi` dep wraps its cargo build in `{{slot_build}}`
    unconditionally, so this is exactly the call the collection-time prebuild
    exists to keep out of per-test budgets."""

    def build():
        _ensure_generated_files_fresh()
        if _IS_WINDOWS:
            subprocess.run(
                ["just", "windows-mail-bridge-build"],
                cwd=_repo_root, check=True,
            )
            return str(_repo_root / "target" / "fauna-mail-bridge.exe")
        subprocess.run(
            ["just", "mail-bridge-build"],
            cwd=_repo_root, check=True,
        )
        return str(_repo_root / "bins" / "fauna-bridges" / "fauna-mail-bridge")

    return _memoized_build("mail-bridge-binary", build)


def _ensure_atproto_bridge_built() -> str:
    """fauna-atproto-bridge via its just recipe; returns the binary path.

    Same `{{slot_build}}`-inside-`mail-bridge-ffi` shape as the mail bridge —
    and the same reason for existing. Until 2026-08-16 the atproto suites ran
    `subprocess.run(["just", "atproto-bridge-build"])` **inside the test body**,
    which is precisely the bound inversion `_prebuild_binaries` was written to
    eliminate: the recipe takes the machine-wide `build` slot unconditionally
    (bounded 5400 s) nested inside `timeout = 900`, so a healthy test dies as a
    bare `Timeout (>900.0s)` whenever the slots are held elsewhere. The
    2026-07-29 fix missed these sites because it is FIXTURE-driven and they
    never requested a fixture — they shelled out directly.

    Measured 2026-08-16: `test_alert_sweep_directory_feeders_e2e.py --app
    windows` timed out at 900 s with the traceback pointing at this very
    `subprocess.run`, while the merge-gate check held `build-0`. It reads
    exactly like the product regression that suite is being investigated for
 — which is the whole hazard.
    """

    def build():
        _ensure_generated_files_fresh()
        if _IS_WINDOWS:
            # Windows has no rpath/LD_LIBRARY_PATH equivalent, so the recipe
            # stages the exe + its fauna_ffi.dll/libunwind.dll in target/,
            # not bins/fauna-bridges/ like the Unix flavor.
            subprocess.run(
                ["just", "windows-atproto-bridge-build"],
                cwd=_repo_root, check=True,
            )
            return str(_repo_root / "target" / "fauna-atproto-bridge.exe")
        subprocess.run(
            ["just", "atproto-bridge-build"],
            cwd=_repo_root, check=True,
        )
        return str(_repo_root / "bins" / "fauna-bridges" / "fauna-atproto-bridge")

    return _memoized_build("atproto-bridge-binary", build)


def _ensure_atproto_bridge_e2e_built() -> str:
    """The e2e FLAVOR of fauna-atproto-bridge (`-tags
    fauna_e2e_seize,fauna_e2e_fixtures`); returns its binary path.

    A **distinct** memo key from `_ensure_atproto_bridge_built` on purpose —
    this is a different binary at a different path, carrying the one-shot
    hostile-rotation seam `test_atproto_custody_alarm.py` needs to drive both
    roles (the long-lived bridge and the seizure) from the same process, so the
    process that signs the seizure is by construction the one that minted the
    genesis — and, since 2026-09-13, the three harness redirect seams
    (`FAUNA_ATPROTO_PLC_DIRECTORY_URL`, `FAUNA_ATPROTO_FAKE_DNS_URL`,
    `FAUNA_ATPROTO_PROXY_FIXTURES`) every fake-directory / fake-DNS / fake-AppView
    test points the bridge through. Convention 15 requires the production
    recipe's own build to carry no trace of any of them, which is exactly why
    this can never collapse onto `atproto-bridge-binary`.
    """

    def build():
        _ensure_generated_files_fresh()
        if _IS_WINDOWS:
            # Same gnullvm-slice reasoning as the production flavor: Go's cgo
            # cannot link the MSVC-built fauna_ffi.dll.
            subprocess.run(
                ["just", "windows-atproto-bridge-build-e2e"],
                cwd=_repo_root, check=True,
            )
            return str(_repo_root / "target" / "fauna-atproto-bridge-e2e.exe")
        subprocess.run(
            ["just", "atproto-bridge-build-e2e"],
            cwd=_repo_root, check=True,
        )
        return str(_repo_root / "bins" / "fauna-bridges" / "fauna-atproto-bridge-e2e")

    return _memoized_build("atproto-bridge-e2e-binary", build)


def _ensure_seal_helper_built() -> str:
    """The TEST-ONLY seal-helper via its just recipe; returns the binary path.
    Same `{{slot_build}}`-inside-`mail-bridge-ffi` shape as the mail bridge."""

    def build():
        _ensure_generated_files_fresh()
        if _IS_WINDOWS:
            subprocess.run(
                ["just", "windows-seal-helper-build"],
                cwd=_repo_root, check=True,
            )
            return str(_repo_root / "target" / "seal-helper-testonly.exe")
        subprocess.run(
            ["just", "seal-helper-build"],
            cwd=_repo_root, check=True,
        )
        return str(
            _repo_root / "bins" / "fauna-bridges" / "seal-helper-testonly"
        )

    return _memoized_build("seal-helper-binary", build)


@pytest.fixture(scope="session")
def mail_bridge_binary(_generated_files_fresh):
    """Build fauna-mail-bridge once per session.

    Unix: `just mail-bridge-build` (CGO_LDFLAGS / LD_LIBRARY_PATH against
    libfauna_ffi.{so,dylib}), returning `bins/fauna-bridges/fauna-mail-bridge`.
    Windows: `just windows-mail-bridge-build` — the gnullvm cgo cross-build that
    emits `target/fauna-mail-bridge.exe` with `fauna_ffi.dll` + `libunwind.dll`
    staged beside it (the same recipe the Windows-native CalDAV/IMAP serving e2e
    uses). Returns the absolute path to the built binary.

    Normally a memo replay: `_prebuild_binaries` already ran the build at
    collection time, outside every per-test timeout budget.
    """
    return _ensure_mail_bridge_built()


@pytest.fixture(scope="session")
def atproto_bridge_binary(_generated_files_fresh):
    """Build fauna-atproto-bridge once per session, at COLLECTION time.

    Unix: `just atproto-bridge-build` → `bins/fauna-bridges/fauna-atproto-bridge`.
    Windows: `just windows-atproto-bridge-build` → `target/fauna-atproto-bridge.exe`
    (the gnullvm cgo cross-build, which stages `fauna_ffi.dll` + `libunwind.dll`
    beside the exe because Windows has no rpath equivalent).

    **Request this fixture instead of shelling out to the recipe in a test
    body.** The recipe takes the machine-wide `build` slot unconditionally with
    a 5400 s bound; run inside a test that bound sits inside `timeout = 900`,
    and the test dies as a bare `Timeout (>900.0s)` under ordinary fleet
    contention — a verdict tracking machine load, not the behavior under test
    (`_ensure_atproto_bridge_built`'s docstring carries the 2026-08-16
    measurement). Requesting the fixture moves the build — and its slot wait —
    to `_prebuild_binaries`, where laziness is still exact: a run that selects
    no atproto test never builds the bridge.

    Callers that resolve the binary path themselves may ignore the return
    value; the fixture's contract is that the build has happened.

    **This is the SHIPPED flavor — it ignores every `FAUNA_ATPROTO_*` harness
    seam** (convention 15: they are compiled out, not switched off). A test
    that sets one of those variables requests `atproto_bridge_e2e_binary`
    instead; this fixture is for tests of the shipped flavor itself
    (`test_atproto_bridge_enroll.py`).
    """
    return _ensure_atproto_bridge_built()


@pytest.fixture(scope="session")
def atproto_bridge_e2e_binary(_generated_files_fresh):
    """Build the e2e FLAVOR of fauna-atproto-bridge once per session, at
    COLLECTION time.

    Unix: `just atproto-bridge-build-e2e` →
    `bins/fauna-bridges/fauna-atproto-bridge-e2e`. Windows:
    `just windows-atproto-bridge-build-e2e` →
    `target/fauna-atproto-bridge-e2e.exe`.

    **A distinct fixture from `atproto_bridge_binary`, never a substitute for
    it** — this flavor carries the one-shot hostile-rotation seam
    (`-tags fauna_e2e_seize`) `test_atproto_custody_alarm.py` needs and the
    three harness redirect seams (`-tags fauna_e2e_fixtures`:
    `FAUNA_ATPROTO_PLC_DIRECTORY_URL`, `FAUNA_ATPROTO_FAKE_DNS_URL`,
    `FAUNA_ATPROTO_PROXY_FIXTURES`) every test that points the bridge at a fake
    directory, fake DNS or fake AppView sets — so **any test that sets one of
    those variables requests THIS fixture**; convention 15 requires the
    production recipe's own build to carry no trace of any of them. Same
    collection-time rationale as `atproto_bridge_binary`: the
    recipe takes the machine-wide `build` slot unconditionally with a 5400 s
    bound, which used to sit inside a test's `timeout = 900` and die as a bare
    `Timeout (>900.0s)` under fleet contention.
    """
    return _ensure_atproto_bridge_e2e_built()


@pytest.fixture(scope="session")
def seal_helper_binary(_generated_files_fresh):
    """Build the TEST-ONLY seal-helper once per session.

    The seal-helper (`bins/fauna-bridges/cmd/seal-helper-testonly`,
    built by `just seal-helper-build`) performs the admin/user client-side
    wrapped-blob seal step the Python harness can't do in-process — it
    wraps the same shared-Rust `seal_*_blob` FFI the Fauna app UI uses.
    See its package doc; it never ships in a deployment artifact.

    Windows: `just windows-seal-helper-build` — the same gnullvm cgo module as
    windows-mail-bridge-build, emitting `target/seal-helper-testonly.exe` with
    `fauna_ffi.dll` + `libunwind.dll` staged beside it.

    Normally a memo replay: `_prebuild_binaries` already ran the build at
    collection time, outside every per-test timeout budget.
    """
    return _ensure_seal_helper_built()


@pytest.fixture(scope="session")
def run_seal_helper(seal_helper_binary):
    """A callable ``(subcommand, params) -> blob_bytes`` over the test-only
    seal-helper, with ``LD_LIBRARY_PATH`` pointed at ``libfauna_ffi.so``.

    The ``mail_bridge_{mta,mda}`` fixtures call the module-level
    ``_run_seal_helper`` directly; this fixture exposes the same client-side
    seal to tests that provision a mail user outside those fixtures — the
    docker deploy round-trip stands in for the user's primary client (derive
    the MSEK-recipient pubkey, seal the wrapped-MSEK AUTH credential + MLS
    snapshot) against a *containerized* nest.
    """
    def _call(subcommand: str, params: dict) -> bytes:
        return _run_seal_helper(seal_helper_binary, subcommand, params)
    return _call


def _run_seal_helper(seal_helper_bin: str, subcommand: str, params: dict) -> bytes:
    """Subprocess the seal-helper for one client-side seal; return blob bytes.

    LD_LIBRARY_PATH points the dynamic linker at libfauna_ffi.so (the
    binary also carries an rpath, but some shells strip RUNPATH).
    """
    env = os.environ.copy()
    _apply_bridge_ffi_env(env)
    proc = subprocess.run(
        [seal_helper_bin, subcommand],
        input=json.dumps(params).encode(),
        capture_output=True,
        env=env,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"seal-helper {subcommand} failed (exit {proc.returncode}): "
            f"{proc.stderr.decode(errors='replace')}"
        )
    import base64 as _b64
    return _b64.b64decode(proc.stdout.strip())


def _published_dkim_record(nest_instance, domain: str, selector: str = "default") -> str:
    """The DKIM TXT value (``v=DKIM1; k=ed25519; p=…``) the nest publishes for
    ``<selector>._domainkey.<domain>``.

    The nest holds each mail domain's DKIM key — minted when the domain is added
    — and signs at the outbound hand-out (`mail-bridge-lifecycle.md` § DKIM
    provisioning (automatic)). A fixture reads the record back here, the value an
    admin would put in DNS, for a test's dkimpy ``dnsfunc``. A missing record
    fails here, loudly, rather than as an unverifiable signature in whichever
    test runs first.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as admin_ws:
        selectors = admin_ws.call(
            "fauna.bridges.list_dkim_selectors", {"domain": domain}
        )["selectors"]
    records = [r["public_dns_value"] for r in selectors if r["selector"] == selector]
    assert len(records) == 1 and records[0], (
        f"the nest publishes no DKIM record for {domain!r} selector {selector!r} "
        f"(got {selectors!r}) — adding a mail domain must mint its nest-held key"
    )
    return records[0]


# The single primary mail domain shared by every mail-bridge fixture spawned
# against one nest. A nest has EXACTLY ONE `is_primary=true` domain
# (`mail-multidomain.md` § The primary domain — partial-unique-index enforced),
# and BOTH bridge roles key their TLS cert on it (`cmd/fauna-mail-bridge/
# main.go:500`). The standalone `mail_bridge_mta` / `mail_bridge_mda` fixtures
# are `scope="session"`, so a full-suite run instantiates both against the one
# session nest; they MUST claim the same domain or whichever lands second gets a
# non-primary domain whose cert blob never matches the runtime `PrimaryDomain`
# → every TLS handshake on that bridge fails `INTERNAL_ERROR`
# (tracked internally). A neutral deployment domain — not the role-named
# `mta.`/`mda.` split that caused the bug — matches the production shape.
# The autouse `_session_primary_mail_domain` fixture below claims this domain
# eagerly on `nest_instance`, so the session's primary is stable regardless of
# which test runs first.
MAIL_PRIMARY_DOMAIN = "fauna.test"
# The handle domain `web_hosting_nest` registers actors under. A real
# DNS-shaped name (not the shared nest's empty domain) so per-user subdomain
# hosting composes `https://<handle>.web.test/` — a host the serving layer's
# subdomain resolver can strip and match. Distinct from MAIL_PRIMARY_DOMAIN so a
# mail-domain change can never silently move where published posts serve.
WEB_HOSTING_DOMAIN = "web.test"
# A second (non-primary) local domain the `mail_bridge_mta` fixture also claims
# — the nest mints its OWN DKIM key for it — so the per-domain From-domain
# DKIM-selection tier_3 test (test_submission_dkim_per_domain_*) can prove
# each domain's outbound is signed with its own key (mail-multidomain.md
# § Signing-key selection at outbound time). Non-primary, so it never disturbs
# the primary's EHLO/MX/TLS/DMARC-alignment behaviour the other tests assert.
MAIL_SECOND_DOMAIN = "second.test"


def _live_nest_session(request) -> bool:
    """True when EVERY selected test carries the ``live_nest`` marker — a
    session whose every nest interaction targets a LIVE remote nest (signed
    into through the UI via DoH discovery, e.g. example.com).

    **This is the marker path, and `--nest live` is deliberately NOT it.** The
    two look alike and are not: a marker session bypasses ``nest_instance``
    entirely and signs clients into a box the harness knows nothing about, while
    the axis RESOLVES ``nest_instance`` through ``_LiveProvider`` — which is what
    gives that run an admin identity, account-scoped provisioning and the
    teardown reap. Folding the axis in here would silently disable all three. The
    one thing they shared — never paying for a local ``fauna-nest`` build — is
    now the mode's own property (``nest_mode.builds_local_nest``), so the axis
    needs nothing from this predicate.

    The marker path stays because the operator-driven tri-machine round
    (``test_filesync_multiseat_live.py``) is invoked without the flag and its
    explicit-``go`` ceremony is untouched by the axis (``testing.md``
    § convention 16).

    Such a session builds/starts NO local ``fauna-nest``: ``_driver_cache``
    launches clients pointed at the live URL instead of resolving
    ``nest_instance``, and ``_session_primary_mail_domain`` skips its pin.
    Found live during the first tri-machine multiseat filesync run: every seat
    paid a cold ~15-25 min nest build for a local nest the test then never
    touched. A MIXED selection (any unmarked test) keeps the normal local-nest
    path — the nest is being built anyway. Session-wide for the same reason as
    ``_installed_windows_app_override``: the drivers are session-scoped, so
    run live_nest suites in their own pytest invocation.
    """
    items = request.session.items
    return bool(items) and all(
        item.get_closest_marker("live_nest") is not None for item in items
    )


# Fixtures whose presence in a test's closure implies the session's LOCAL nest.
# `app` / `persistent_app` / `fresh_app` matter because `_driver_cache` resolves
# `nest_instance` LAZILY now (see `_live_nest_session`): plain-`app` tests no
# longer carry `nest_instance` in their declared closure, yet a non-live session
# of them still launches every app against the local nest.
#
# `nest_mode` is the STRUCTURAL member, and it subsumes the other four: it is
# the one name every fixture that starts a harness-owned nest necessarily
# requests, because it must hand it to `_start_dedicated_nest` to pick a
# provider. `nest_instance` takes it, `_driver_cache` takes it (which is how
# plain-`app` tests reach the nest), and so does every dedicated-nest fixture
# arm 1 routes — in conftest and in test modules alike.
#
# It is here because the hand-maintained half failed exactly as a hand-maintained
# list does (measured 2026-08-30). Routing moves the binary out of a fixture's
# DECLARED closure into a lazy `getfixturevalue`, so the prebuild is the only
# thing keeping the build outside `timeout = 900`. The module-local fixtures
# routed the day before were covered by accident — every one is an app journey,
# so `app` matched — but the `tests/api/` family is not: pure API tests, no
# driver, nothing in the old four matched. A real-path run of the seven routed
# files died `Failed: Timeout (>900.0s)` inside the FIRST fixture's
# `build_node()` with the machine's build queue six deep, and the other 39 tests
# errored on the same memoized failure — the precise bound inversion
# `_prebuild_binaries` exists to prevent, wearing a shape that reads like a
# product bug. Keyed on `nest_mode`, a fixture routed in future is covered by
# being routed. `test_nest_mode_axis.py` pins both halves.
_LOCAL_NEST_FIXTURE_USERS = (
    "nest_mode", "nest_instance", "app", "persistent_app", "fresh_app",
)

# The same bound-inversion `_LOCAL_NEST_FIXTURE_USERS` corrects for
# `nest_binary`, but for `mail_bridge_binary`/`seal_helper_binary`: every one
# of these fixtures reaches `_start_mail_venue` -> the standalone provider's
# `start_mail_venue`, which fetches both via `request.getfixturevalue(...)`
# rather than a declared parameter (mode-polymorphic — docker's own provider
# needs neither, so a declared param would over-build there). A dynamic
# `getfixturevalue` call is invisible to pytest's static `item.fixturenames`,
# so `_prebuild_binaries`'s closure scan never sees these two names for a run
# whose only mail-venue fixture is one of these four — the first such test
# pays the cold build inside its own 900 s budget instead. Measured
# 2026-09-09: `test_addressbook.py::test_address_book_lists_and_details_mda_sealed_card[web]`
# died as a bare `Timeout (>900.0s)` on `subprocess.run(["just",
# "mail-bridge-build"], ...)`, reading exactly like a product bug — the same
# failure shape `_prebuild_binaries`'s own docstring already documents for
# `test_addressbook.py --app tui` on 2026-07-29, which this fixture set closes
# for good. `dedicated_caldav_admin_port_nest` is NOT here: it declares
# `mail_bridge_binary`/`seal_helper_binary` as real parameters, so the static
# closure already covers it.
_MAIL_VENUE_FIXTURE_USERS = frozenset({
    "dedicated_mail_nest", "dedicated_mail_nest_handle_domain",
    "dedicated_caldav_mailbox_less_nest", "dedicated_caldav_only_nest",
})


# A directly-parametrized argname sits in `fixturenames` beside real fixture
# requests, so every scan below keys on the FIXTUREDEF instead of the name —
# `helpers/fixture_closure.py` carries the full statement and the incident that
# forced it (a parametrized `app` wedged the tier_1 declarations suite behind a
# cold nest build). Re-exported under the module's private names because the
# gate suite pins them here.
from helpers.fixture_closure import is_real_fixture as _is_real_fixture
from helpers.fixture_closure import real_fixture_closure as _real_fixture_closure


def _session_uses_local_nest(items) -> bool:
    """Does any collected test's fixture closure imply the local ``nest_instance``?"""
    return any(
        any(
            name in getattr(item, "fixturenames", ()) and _is_real_fixture(item, name)
            for name in _LOCAL_NEST_FIXTURE_USERS
        )
        for item in items
    )


@pytest.fixture(scope="session", autouse=True)
def _session_primary_mail_domain(request):
    """Pin `MAIL_PRIMARY_DOMAIN` as the session nest's primary mail domain.

    Nest derives `is_primary` from "first active domain wins"
    (`bins/fauna-nest/src/bridge_routing_handlers.rs::add_local_domain_handler`,
    `list_active_mail_domains().is_empty()`). Without an upfront claim, the
    first test that calls `fauna.bridges.add_local_domain` (often a tier_2
    DNS-admin test like `test_admin_dns_managed::_seed_local_domain`) wins the
    primary slot. When the lazy session-scoped `mail_bridge_mta` /
    `mail_bridge_mda` fixtures spawn later they provision their TLS cert blob
    keyed `(role, bridge_id, MAIL_PRIMARY_DOMAIN)`, but the running bridge
    anchors its TLS provider on `snapshot.PrimaryDomain`
    (`cmd/fauna-mail-bridge/main.go:501`) — now the test's random domain.
    `FetchTLSCertBlob` returns "no blob for this domain", the cached cert
    stays nil, and every STARTTLS handshake on port 25 fails with
    `TLSV1_ALERT_INTERNAL_ERROR` (this is the same failure documented
    internally for the two-bridge collision; same root cause, different
    trigger).

    Mirrors production where the admin claims a primary mail domain at
    onboarding (the `mail_domain` carried by `claim_admin`, memory
    `onboarding-auto-adds-handle-domain`) — not "first call wins".
    """
    # Only the shared LOCAL `nest_instance` needs an upfront primary-domain pin.
    # Sessions that never touch it — tier_1 unit tests, tier_4 docker tests
    # that spin their own containerized nest, and `live_nest` sessions that
    # target a live remote box — must NOT drag in `nest_instance` (it builds
    # fauna-nest via cargo/just): wasted work on a dev box, and fatal on a
    # docker-only CI runner, which has no rust/just toolchain. Gate on whether
    # any collected test actually uses the local nest — including via the
    # driver launch config (`_LOCAL_NEST_FIXTURE_USERS`), since `_driver_cache`
    # resolves the nest lazily and plain-`app` closures no longer name it.
    # (Resolve it lazily via getfixturevalue so this autouse fixture doesn't
    # itself inject `nest_instance` into every test's fixture closure — that
    # would defeat the check.)
    if _live_nest_session(request):
        return
    # Live is the third exclusion, and the sharpest: claiming a primary mail
    # domain on a real deployed box is a GLOBAL mutation a human would feel, and
    # this fixture is autouse — so it must gate itself here rather than exclude
    # every test on the axis (`nest_surface.LIVE_SELF_GATED_FIXTURES` records the
    # carve-out, and the AST pin stops a second fixture joining it silently). A
    # live box already has its primary domain; there is nothing to pin.
    from helpers import nest_mode as nest_mode_mod

    if nest_mode_mod.run_mode().is_live:
        return
    if not _session_uses_local_nest(request.session.items):
        return
    nest_instance = request.getfixturevalue("nest_instance")

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    if admin is None:
        # `unclaimed_nest_instance` and friends leave admin=None — those nests
        # exist to test the client-driven claim path itself and never spawn
        # a mail bridge, so no primary is needed.
        return
    ws = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with ws:
        ws.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": MAIL_PRIMARY_DOMAIN,
                "mta_sts_cert_mode": "per_host",
            },
        )


@pytest.fixture(autouse=True)
def _actuation_log_test_boundary(request):
    """Under `--actuation-log PATH`, stamp `=== TEST <nodeid>` into the log before
    each test, so every `DISABLED-ACTUATION` marker the app appends is attributed
    to the test that caused it.

    The app cannot do this itself: it is launched per test *module* (the cold
    relaunch of testing.md convention 10) and never learns which test is running.
    Writing the boundary from the harness side keeps the app's half a dumb
    append, and makes a whole-suite sweep's output directly actionable — each
    offender arrives with the test to triage it against, rather than a timestamp
    to correlate by hand across 1600 tests. No-op without the flag."""
    log_path = request.config.getoption("--actuation-log", default=None)
    if log_path:
        try:
            with open(Path(log_path).expanduser().resolve(), "a") as fh:
                fh.write(f"=== TEST {request.node.nodeid}\n")
        except OSError:
            # Same best-effort contract as the app's appender: a harness sink
            # that cannot be written must never fail the test it is observing.
            pass
    yield


def _live_box_opt_in_reason(node, live_mode):
    """Return a skip reason when `node` drives the SHARED live box without an
    explicit opt-in, else None.

    A `live_box` test is destructive-by-default against a box other people are
    using (pytest.ini's marker text), so reaching one by accident is the whole
    hazard. The opt-in is satisfied two ways, both of which mean an operator
    deliberately asked for the live box:

      * ``FAUNA_E2E_LIVE=1`` — the convention `tests/live/conftest.py` already
        calls "REQUIRED explicit opt-in (belt-and-suspenders)", and the one the
        three `just e2e-live-*` recipes export for you.
      * ``--nest live`` — naming the box on the command line IS the request;
        demanding the env var on top would be redundant friction on an already
        fragile three-machine ceremony.

    Why the gate is keyed on the MARKER rather than hand-rolled per module: the
    two tri-machine multiseat modules gated on "does this machine have an
    account seed" (`~/.fauna-id`) instead, which is true on every dev machine,
    so a plain `pytest tests/e2e-unified/tests/ --app tui` collected and ran
    their live steps unattended — filed twice as regressions on "do NOT reopen"
    items when they were neither regressions nor flakes. Putting the guarantee on the marker means the next
    live module inherits it instead of re-deriving it. Pinned by
    `tests/test_live_box_opt_in_gate.py`.
    """
    if node.get_closest_marker("live_box") is None:
        return None
    if live_mode:
        return None
    if os.environ.get("FAUNA_E2E_LIVE", "").strip().lower() in {"1", "true", "yes", "on"}:
        return None
    return (
        "live_box test: drives the SHARED live nest and is opt-in. Set "
        "FAUNA_E2E_LIVE=1 (or pass `--nest live`) to run it. Three-machine "
        "rounds additionally need their operator handshake — see "
        "e2e-conventions.md convention 16 and the module docstring."
    )


@pytest.fixture(autouse=True)
def _live_box_opt_in_gate(request):
    """Skip every `live_box` test unless the explicit live opt-in is present.

    Ordered BEFORE `_serialize_live_box` so a gated test never takes the
    machine-wide box flock on its way to being skipped. Decision logic lives in
    `_live_box_opt_in_reason` (a plain function, so it is unit-pinnable without
    spawning pytest)."""
    # Fast path for the ~1600 tests that are not live_box: check the marker
    # before resolving the nest mode, so an autouse fixture on every test in
    # the suite costs one `get_closest_marker` call.
    if request.node.get_closest_marker("live_box") is None:
        return
    from helpers import nest_mode as nest_mode_mod

    reason = _live_box_opt_in_reason(
        request.node, live_mode=nest_mode_mod.run_mode().is_live
    )
    if reason:
        pytest.skip(reason)


@pytest.fixture(autouse=True)
def _serialize_live_box(request):
    """For `live_box`-marked tests, hold a machine-wide flock on the shared live
    nest (`FAUNA_LIVE_NEST_URL`) for the test's duration, so two sessions'
    live tests against the same box serialize instead of colliding on its
    claim/reset state. No-op for every other test, and for a live test whose
    env isn't set (it skips on its own). See `helpers/live_box_lock.py` and
    memory `live-box-e2e-pkill-faunadesktop-hazard`.

    **`--nest live` engages it too, and that is the point of folding live under
    the axis**: the flock used to depend on a test remembering the `live_box`
    marker AND on `FAUNA_LIVE_NEST_URL` being exported, so a run that reached the
    shared box any other way was simply unserialized. The mode is a run-level
    fact, so it cannot be forgotten per-test."""
    from helpers import nest_mode as nest_mode_mod

    mode = nest_mode_mod.run_mode()
    # In live mode, engage for the tests that actually reach a nest — the same
    # `nest_mode`-in-the-closure predicate `pytest_generate_tests` stamps ids by.
    # A tier_1 unit test in a live run touches no box, and taking a machine-wide
    # flock for it would serialize siblings against nothing.
    mode_serializes = mode.is_live and "nest_mode" in request.fixturenames
    if not mode_serializes and request.node.get_closest_marker("live_box") is None:
        yield
        return
    nest_url = os.environ.get("FAUNA_LIVE_NEST_URL", "").strip()
    if mode_serializes:
        # The mode's own target wins: `--nest live:URL` names the box being hit,
        # and locking a *different* URL than the one under test serializes
        # nothing. Falls back to the env/default the same way the provider does
        # — the same function, so the two cannot disagree.
        nest_url = nest_mode_mod.live_url(mode)
    if not nest_url:
        yield
        return
    from helpers import live_box_lock

    fd = live_box_lock.acquire(nest_url)
    try:
        yield
    finally:
        live_box_lock.release(fd)


def _generate_self_signed_cert(domain: str) -> tuple[bytes, bytes]:
    """Return (cert_chain_pem, priv_key_pem) for a self-signed EC P-256
    cert covering `domain` — enough for the bridge's implicit-TLS
    submission listener to bind (the test client trusts it via CERT_NONE).
    """
    import datetime
    from cryptography import x509
    from cryptography.x509.oid import NameOID
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec

    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, domain)])
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(days=3650))
        .add_extension(x509.SubjectAlternativeName([x509.DNSName(domain)]), critical=False)
        .sign(key, hashes.SHA256())
    )
    cert_pem = cert.public_bytes(serialization.Encoding.PEM)
    key_pem = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    )
    return cert_pem, key_pem


class MailBridgeMTAHandle:
    """Handle returned by the `mail_bridge_mta` fixture.

    Exposes the bridge's ephemeral bind ports, the deployment's
    primary local domain, the pre-provisioned recipient actor + its
    local-part, the keypair file path (for E.3's TLS-cert
    provisioning add-on), and a TLS trust anchor placeholder
    (populated in E.3 once admin-uploaded TLS lands).
    """

    def __init__(
        self,
        proc,
        mx_port,
        submission_port_465,
        submission_port_587,
        metrics_port,
        domain,
        bridge_id,
        bridge_role,
        recipient_local_part,
        recipient_actor,
        keypair_file,
        log_file,
        tls_trust_anchor,
        ed25519_pubkey,
        x25519_pubkey,
        submission_credential,
        submission_sender_local,
        stub_mx,
        submission_sender_owned_alias_local=None,
        submission_sender_actor=None,
        nest_url=None,
        tls_stub_mx=None,
        dane_domain=None,
        dkim_selector=None,
        dkim_public_dns_value=None,
        second_domain=None,
        second_dkim_selector=None,
        second_dkim_public_dns_value=None,
    ):
        self.proc = proc
        self.mx_port = mx_port
        self.submission_port_465 = submission_port_465
        self.submission_port_587 = submission_port_587
        self.metrics_port = metrics_port
        self.domain = domain
        self.bridge_id = bridge_id
        self.bridge_role = bridge_role
        self.recipient_local_part = recipient_local_part
        self.recipient_actor = recipient_actor
        self.keypair_file = keypair_file
        self.log_file = log_file
        # The self-signed cert PEM the submission listener (465) serves;
        # the round-trip test connects with CERT_NONE so it's informational.
        self.tls_trust_anchor = tls_trust_anchor
        self.ed25519_pubkey = ed25519_pubkey
        self.x25519_pubkey = x25519_pubkey
        # E.3 submission round-trip inputs: the AUTH PLAIN credential
        # (bytes), the sender's local part (AUTH username = sender@domain),
        # and the in-process stub external MX the bridge delivers to.
        self.submission_credential = submission_credential
        self.submission_sender_local = submission_sender_local
        # A second address the sender actor owns whose local-part ≠ the login
        # handle (`submission_sender_local`). A submission test that sets
        # MAIL FROM to `<this>@<domain>` drives the resolver-backed owned-alias
        # acceptance branch (`assertMailFromOwned` → resolve_recipient →
        # actor_id match), which the login-handle fast-path bypasses. `None`
        # on bridges provisioned without it (e.g. the disposable drain bridge).
        self.submission_sender_owned_alias_local = submission_sender_owned_alias_local
        # The full sender-actor object (`_create_actor` dict, incl.
        # `actor_id_bytes`) — lets a submission test assert server-side sealed
        # delivery back to the sender (e.g. a permanent-failure DSN sealed into
        # the sender's INBOX; the in-domain-sender bounce delivers locally, never
        # over the MX). Mirrors `recipient_actor`. `None` on bridges provisioned
        # without a sender actor.
        self.submission_sender_actor = submission_sender_actor
        self.stub_mx = stub_mx
        # Base URL of the nest this bridge is enrolled with — lets tier_3
        # tests reach nest's `--features test-hooks` endpoints (e.g.
        # `POST /api/v1/test/outbound/clock` to fast-forward the outbound
        # retry clock for the 4 h delay-warning, T1.3).
        self.nest_url = nest_url
        # TLS-capable stub external MX (STARTTLS + self-signed cert) and the
        # recipient domain (`dane.test`) routed at it, for the outbound DANE
        # tests (T2.1b). `tls_stub_mx.cert_der` is the leaf the test hashes
        # into the published TLSA record.
        self.tls_stub_mx = tls_stub_mx
        self.dane_domain = dane_domain
        # DKIM selector + the unsealed public-key TXT value (`v=DKIM1; …`)
        # for `<selector>._domainkey.<domain>`, read back from the nest (which
        # holds the key and signs at the outbound hand-out); lets a test verify
        # the delivered message's DKIM-Signature against this exact published
        # value via a dkimpy `dnsfunc` — see `test_submission_dkim_signature_
        # verifies`. `None` on a handle whose fixture did not read the record.
        self.dkim_selector = dkim_selector
        self.dkim_public_dns_value = dkim_public_dns_value
        # A SECOND (non-primary) local domain with its OWN nest-held DKIM key
        # + selector + published TXT value. Lets the per-domain From-domain
        # DKIM-selection tier_3 test (test_submission_dkim_per_domain_*) submit
        # From: this domain and dkimpy-verify the signature carries THIS
        # domain's d=/s= — proving each local domain is signed with its own
        # key, not the primary's (mail-multidomain.md § Signing-key selection
        # at outbound time). `None` when the fixture read only the primary's
        # record.
        self.second_domain = second_domain
        self.second_dkim_selector = second_dkim_selector
        self.second_dkim_public_dns_value = second_dkim_public_dns_value

    def kill(self):
        """Terminate the bridge process with a 5s grace window then SIGKILL."""
        try:
            self.proc.terminate()
        except Exception:
            return
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


def _wait_for_mail_bridge_metrics(port: int, timeout: float = 30.0) -> None:
    """Poll the bridge's `/healthz` endpoint until 200 OK or timeout.

    The bridge writes `ok\\n` to GET /healthz once metrics + role
    dispatch are running. Anything else is treated as still-starting.
    """
    import urllib.request
    deadline = time.monotonic() + timeout
    last_err = None
    while time.monotonic() < deadline:
        try:
            resp = urllib.request.urlopen(
                f"http://127.0.0.1:{port}/healthz", timeout=1.0
            )
            if resp.status == 200:
                return
        except Exception as e:
            last_err = e
        time.sleep(0.2)
    raise TimeoutError(
        f"mail-bridge /healthz on port {port} did not become ready within "
        f"{timeout}s (last err: {last_err})"
    )


def _wait_for_tcp_accept(port: int, timeout: float = 30.0) -> None:
    """Poll until a TCP connect to 127.0.0.1:port succeeds (listener bound).

    `/healthz` flips to 200 once the metrics mux is serving, which the bridge
    does *before* `role.Run` binds the SMTP listeners (`cmd/fauna-mail-bridge/
    main.go` calls `metricsMux()` then dispatches the role). A healthz-only
    wait therefore races the port-25 bind. This closes the gap for both the
    session bridge and any disposable instance, so a test never connects
    before the listener is up. A bare connect-then-close is a benign client
    disconnect on a `STARTTLS`-required port 25.
    """
    deadline = time.monotonic() + timeout
    last_err = None
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1.0):
                return
        except OSError as e:
            last_err = e
            time.sleep(0.1)
    raise TimeoutError(
        f"mail-bridge SMTP listener on 127.0.0.1:{port} not accepting within "
        f"{timeout}s (last err: {last_err})"
    )


def live_admin_token(nest: dict) -> str | None:
    """A bearer that is valid **right now** for ``nest``'s admin, or ``None`` on
    an unclaimed nest.

    ⚠ **Never read ``nest["admin"]["token"]`` directly from a fixture that can be
    set up long after the nest was.** The nest mints bearers with a **one-hour**
    TTL (`bins/fauna-nest/src/auth_core.rs::TOKEN_TTL_SECS = 3600`) and
    `AdminBearerAuth` answers **401** the moment `token_store.validate` says
    expired (`bins/fauna-nest/src/auth.rs`) — but `nest_instance` is
    **session**-scoped, so the token it cached at claim time is dead by the time
    any fixture that first runs past the hour mark uses it.

    That is a *deterministic* failure that presents as a flake, which is why it
    cost several triage passes: solo, the fixture runs minutes after the claim
    and always passes; in a batched sweep the same fixture runs 60+ minutes in
    and every admin HTTP call 401s, surfacing as `ERROR at setup` across the
    whole bridge-dependent family at once (the 2026-08-02 `--app tui` sweep's
    setup-ERROR cluster; that re-run took 1:36:28). Re-minting per use costs one
    anonymous WS handshake on a path that runs a handful of times per session.
    """
    admin = nest.get("admin")
    if not admin:
        return None
    from common.auth import mint_token_via_handshake

    return mint_token_via_handshake(nest["url"], admin["signing_key"])


def _bridge_admin_post(nest_url: str, admin_token: str, path: str, body: dict) -> dict:
    """POST a JSON body to one of nest's HTTP routes as the admin — in practice
    the `/api/v1/test/*` hooks, which have no WS-RPC twin. Bridge enrollment is
    WS-RPC only: `helpers/bridge_enrollment.py`.

    A `serve_tls=True` nest serves these routes over self-signed HTTPS (the
    Pillar-C uniform-https posture); the loopback floor cert never chains to
    WebPKI, so an `https://` base uses an unverified context (the same
    channel-binding-not-WebPKI trust the WS-RPC clients use via `CERT_NONE`).
    A plain-`http://` base passes `context=None` — urllib's default, unchanged.
    """
    import ssl
    import urllib.request

    ctx = ssl._create_unverified_context() if nest_url.startswith("https://") else None
    req = urllib.request.Request(
        f"{nest_url}{path}",
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {admin_token}",
        },
        method="POST",
    )
    resp = urllib.request.urlopen(req, timeout=5.0, context=ctx)
    raw = resp.read()
    return json.loads(raw) if raw else {"_status": resp.status}


class _SpawnedBridge:
    """The process half every spawned bridge shares: the subprocess, its log
    handle, the argv/env that produced it, and the two lifecycle verbs built on
    them — `respawn()` (play supervisor) and `cleanup()`.

    Both roles need identical respawn machinery, so it lives here once rather
    than being copied into each role's class (priority #2/#4). Subclasses
    supply only what differs: `_wait_until_serving`, which decides what
    "serving" means for that role.
    """

    def _init_process(self, *, proc, log_file_path, log_fh, metrics_port,
                      spawn_argv=None, spawn_env=None):
        self.proc = proc
        self.log_file_path = log_file_path
        self.log_fh = log_fh
        self.metrics_port = metrics_port
        # The exact argv + env of the first spawn, captured so `respawn()` can
        # re-launch the identical process (same keyfile + operator-hatch) after
        # a config-driven exit — see `respawn()`.
        self.spawn_argv = spawn_argv
        self.spawn_env = spawn_env

    def _wait_until_serving(self, timeout: float) -> None:
        """Block until this role is serving again. Overridden per role."""
        _wait_for_mail_bridge_metrics(self.metrics_port, timeout=timeout)

    def wait_for_gate_exit(self, timeout: float = 60.0) -> None:
        """Block until the bridge process exits of its own accord.

        The exit under test is the **exit-for-rebind**: a bridge whose gating
        config changed (an admin enable, a CalDAV port move) cancels its
        listeners and returns cleanly so its supervisor restarts it bound to
        the new shape (`mail-bridge-lifecycle.md` § Default-off;
        `internal/wsrpc/idle_gate_watch.go`). A *non-zero* exit is a crash and
        is raised rather than papered over by a respawn.

        The budget is generous and latency-independent (e2e-conventions.md
        convention 14): the wait ends on the process's own state change, not on
        a fixed delay, so a loaded machine costs patience, never a verdict.
        """
        try:
            rc = self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            raise TimeoutError(
                f"bridge {getattr(self, 'bridge_id', '?')} did not exit for rebind within "
                f"{timeout}s of the config change; it is still running. In likelihood "
                f"order: (1) THE GATE WAS NEVER OPENED — if the app was driven as a "
                f"NON-ADMIN actor, its `enable_mail_plain` did NOT flip the "
                f"deployment-wide toggle: `set_mail_enabled` is Admin-class and "
                f"`MailSettingsMachine::enable_mail` swallows the rejection (\"the "
                f"non-admin no-op\"), so `mta.Bindable` never opened and this bridge is "
                f"correctly still idling — call `handle.admin_opens_mail_gate()` before "
                f"`rebind_after_enable()`; (2) the `config_changed` push never reached it "
                f"(is it approved?); (3) its idle gate watcher regressed. Hypothesis 1 "
                f"leaves no trace in the bridge log — the bridge did nothing wrong — so "
                f"rule it out FIRST, before reading {self.log_file_path}"
            ) from None
        if rc != 0:
            raise RuntimeError(
                f"bridge {getattr(self, 'bridge_id', '?')} exited {rc}, not 0 — that is a "
                f"crash, not an exit-for-rebind; see {self.log_file_path}"
            )

    def respawn(self, *, wait_for_serving: bool = True, timeout: float = 60.0) -> None:
        """Re-launch the bridge with the SAME argv after a config-driven exit —
        the "play supervisor" step.

        Production runs each bridge under s6 (`mail-bridge-lifecycle.md`
        § Default-off), which restarts it on a clean exit; the binaries e2e has
        no supervisor, so the fixture does that job here. Because the keyfile is
        unchanged the bridge reconnects as the already-approved service user (no
        re-enroll / re-approve / re-provision — the `bridge_service_users` row +
        sealed cert persist), and because the operator-hatch is unchanged it
        re-reads its config and binds whatever the NEW snapshot enables. Reusing
        the same ephemeral ports is safe: the prior process has fully exited.
        """
        from drivers.port_util import track_process, untrack_process

        if self.spawn_argv is None:
            raise RuntimeError(
                "respawn() needs the spawn argv captured at first spawn "
                "(the _spawn_*_bridge helpers populate it)"
            )
        assert self.proc.poll() is not None, (
            "respawn() requires the prior process to have already exited "
            f"(its config-driven rebind exit); poll()={self.proc.poll()!r}. "
            "Call wait_for_gate_exit() first."
        )
        untrack_process(self.proc)
        try:
            self.log_fh.close()
        except Exception:
            pass
        # Append so the rebind's second-boot logs sit below the first boot's in
        # the same file (a failure tail then shows the whole bind→rebind story).
        self.log_fh = open(self.log_file_path, "ab")
        from drivers.port_util import popen_group_kwargs, reap_descendants_of
        self.proc = subprocess.Popen(
            self.spawn_argv,
            stdout=self.log_fh,
            stderr=subprocess.STDOUT,
            env=self.spawn_env,
            **popen_group_kwargs(),
        )
        # Re-armed on every rebind respawn, not just the first spawn: the job
        # binds a pid, so a new process needs a new assignment (testing.md § 9).
        reap_descendants_of(self.proc.pid)
        track_process(self.proc)
        if wait_for_serving:
            self._wait_until_serving(timeout)

    def rebind(self, *, timeout: float = 60.0) -> None:
        """The whole supervisor act in one call: wait for the exit-for-rebind,
        relaunch, and wait until this role is serving again.

        This is what a test calls after driving an enable through the app UI.
        """
        self.wait_for_gate_exit(timeout=timeout)
        self.respawn(wait_for_serving=True, timeout=timeout)

    def cleanup(self) -> None:
        """Terminate the bridge process (grace then SIGKILL) and untrack it.

        Safe to call after a test has already SIGTERM'd + reaped the process
        (the graceful-shutdown test does): `poll()` short-circuits the signal
        when it has already exited. Only touches *this* bridge's process —
        never a sibling's — and leaves nest-global resources (stub MXes,
        scanners) to the owning fixture.
        """
        from drivers.port_util import untrack_process

        try:
            if self.proc.poll() is None:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
                    self.proc.wait()
        except Exception:
            pass
        untrack_process(self.proc)
        try:
            self.log_fh.close()
        except Exception:
            pass


class _SpawnedMtaBridge(_SpawnedBridge):
    """The per-bridge mechanical result of `_spawn_mta_bridge`.

    Holds only the fields that are unique to one bridge process — its
    enrolled keypair, its ephemeral bind ports, the cert it serves, the
    spawned process + log handle — so a caller can assemble a
    `MailBridgeMTAHandle` around it (mixing in the nest-global state it
    owns) and `cleanup()` the process independently of any other bridge.
    """

    def __init__(
        self,
        *,
        proc,
        bridge_id,
        mx_port,
        submission_port_465,
        submission_port_587,
        metrics_port,
        keyfile_path,
        log_file_path,
        log_fh,
        ed25519_pubkey,
        x25519_pubkey,
        cert_pem,
        tls_blob=None,
        spawn_argv=None,
        spawn_env=None,
    ):
        self._init_process(
            proc=proc,
            log_file_path=log_file_path,
            log_fh=log_fh,
            metrics_port=metrics_port,
            spawn_argv=spawn_argv,
            spawn_env=spawn_env,
        )
        self.bridge_id = bridge_id
        self.mx_port = mx_port
        self.submission_port_465 = submission_port_465
        self.submission_port_587 = submission_port_587
        self.keyfile_path = keyfile_path
        self.ed25519_pubkey = ed25519_pubkey
        self.x25519_pubkey = x25519_pubkey
        self.cert_pem = cert_pem
        # The sealed TLS-cert blob (sealed to this bridge's x25519). Always set
        # so a caller that spawned with `pre_approve=False` (unclaimed nest, no
        # admin yet) can `provision_tls_cert_blob(it)` once it has claimed the
        # nest and holds the admin key.
        self.tls_blob = tls_blob

    def _wait_until_serving(self, timeout: float) -> None:
        """An MTA is serving when its SMTP listener accepts — not merely when
        `/healthz` is up.

        `main.go` serves the metrics mux BEFORE dispatching the role, so a
        metrics-only wait is satisfied by a bridge that went on to idle and bind
        nothing. That gap is exactly what let the `dedicated_mail_nest` MDA look
        healthy while serving nothing for a week, so the MTA's wait
        anchors on the real listener.
        """
        _wait_for_mail_bridge_metrics(self.metrics_port, timeout=timeout)
        _wait_for_tcp_accept(self.mx_port, timeout=timeout)


def _spawn_mta_bridge(
    *,
    mail_bridge_binary,
    seal_helper_binary,
    nest_instance,
    tmp,
    bridge_id,
    domain,
    bridge_role="mta",
    operator_hatch_extra="",
    pre_approve=True,
    wait_for_serving=True,
):
    """Spawn one MTA-role `fauna-mail-bridge` process against `nest_instance`.

    Owns *only* the work that two bridges can never share: a fresh
    service-user keypair + its admin enrollment (so the bridge's nest WS
    actor is unique — two bridges on one keypair would collide on the actor),
    a TLS cert blob sealed to that keypair (so port 25 can advertise the
    `STARTTLS` the inbound tests require), the four ephemeral bind ports, the
    operator-hatch, and the spawned subprocess. No DKIM: the nest holds every
    mail domain's key and signs at the outbound hand-out.

    Nest-global deployment state — the primary local domain, the recipient /
    sender actors + their aliases + MLS pubkeys, the spam policy, the
    submission token, the in-process stub MXes / scanners — is the *caller's*
    responsibility and is provisioned once, then reused read-only across
    bridges. That separation is what lets the destructive graceful-shutdown
    test stand up its own disposable bridge and SIGTERM it without stranding
    the session-scoped bridge every other test in the file shares.

    `operator_hatch_extra` is appended verbatim to the generated hatch TOML
    (after the four bind addrs) so the caller can wire `clamd_addr` /
    `rspamd_url` / an `[mta_mx_override]` block onto the bridge that needs
    them; a disposable drain-only bridge passes "".

    Returns a `_SpawnedMtaBridge`; the caller wraps it in a
    `MailBridgeMTAHandle` and calls `.cleanup()` on teardown.
    """
    import time as _time
    import base64 as _b64
    from nacl.signing import SigningKey as NaClSigningKey
    from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
    from cryptography.hazmat.primitives.serialization import (
        Encoding, NoEncryption, PrivateFormat, PublicFormat,
    )
    import cbor2
    from drivers.port_util import find_free_port, find_free_ports, track_process, untrack_process
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.bridge_enrollment import enroll_and_approve_bridge

    nest_url = nest_instance["url"]
    # `admin` is None on an unclaimed nest (pre_approve=False) — the
    # pre-approve + WS-RPC cert provisioning below are gated on `pre_approve`.
    admin = nest_instance["admin"]

    # ── 1. Service-user keypair (Ed25519 signing + X25519 wrap-target).
    ed_sk = NaClSigningKey.generate()
    ed_seed = bytes(ed_sk)
    ed_pubkey = bytes(ed_sk.verify_key)

    x_sk = X25519PrivateKey.generate()
    x_priv = x_sk.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption())
    x_pubkey = x_sk.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)

    # Keyfile format mirrors `libs/fauna-mls/src/wrapped_blob/
    # service_user.rs::ServiceUserKeyfile` and the Go decoder at
    # `bins/fauna-bridges/internal/keypair/keyfile.go` (decoded-value
    # parity). Six keys: v, role, bridge_id, ed25519_seed, x25519_priv,
    # created_at. `canonical=True` → DAG-CBOR length-first key order (the Go
    # keyfile decoder rejects non-canonical maps).
    keyfile_bytes = cbor2.dumps({
        "v": 1,
        "role": bridge_role,
        "bridge_id": bridge_id,
        "ed25519_seed": ed_seed,
        "x25519_priv": x_priv,
        "created_at": int(_time.time()),
    }, canonical=True)
    # Name the keyfile `<role>.key` (mta.key / mda.key) to mirror production
    # (docker/s6 `/data/keys/{mta,mda}.key`): the bridge's zero-touch
    # self-enrollment derives its `role_hint` from the keyfile *basename*
    # (`roleHintFromKeyfile` → `request_enrollment`), and nest rejects any
    # role_hint that isn't "mta"/"mda" (`bridge_blob_handlers.rs`). A generic
    # name like `mail-bridge.key` would self-enroll as role_hint="mail-bridge"
    # → ok=false → the bridge exits before binding.
    keyfile_path = tmp / f"{bridge_role}.key"
    keyfile_path.write_bytes(keyfile_bytes)
    keyfile_path.chmod(0o600)

    # ── 2. Admin pre-approves the bridge's pubkey: the harness makes the
    # bridge's own `request_enrollment` call for it, then the admin's
    # `approve_pending_bridge`. Approval also inserts the audit-only `users`
    # row, so the bridge's challenge-response auth resolves without a separate
    # register_user.
    # Skipped when `pre_approve=False`: the bridge then cold-boots into its
    # zero-touch `request_enrollment` poll (a PENDING row) and an admin
    # approves it later — e.g. through the client's admin-bridges-pending UI.
    if pre_approve:
        enroll_and_approve_bridge(
            nest_url, admin["signing_key"], ed_pubkey, bridge_role, bridge_id,
        )

    # ── 3. Seal the admin-uploaded TLS cert to *this* bridge's ephemeral
    # X25519 pubkey via the seal-helper, then provision over Admin-class
    # WS-RPC. TLS is keyed (role, bridge_id, domain) on the bridge's fetch, so
    # a distinct bridge_id never clobbers a sibling bridge's blob.
    _now = int(_time.time())
    x_pubkey_b64 = _b64.b64encode(x_pubkey).decode()
    cert_pem, key_pem = _generate_self_signed_cert(domain)
    tls_blob = _run_seal_helper(seal_helper_binary, "seal-tls-cert", {
        "bridge_role": bridge_role,
        "bridge_id": bridge_id,
        "domain": domain,
        "recipient_x25519_pubkey_b64": x_pubkey_b64,
        "cert_chain_pem_b64": _b64.b64encode(cert_pem).decode(),
        "priv_key_pem_b64": _b64.b64encode(key_pem).decode(),
        "issued_at": _now,
        "expires_at": _now + 3650 * 86400,
    })
    # Provision the sealed blob over Admin WS-RPC. Skipped when there's no
    # admin yet (`pre_approve=False`, unclaimed nest) — the sealed `tls_blob`
    # is returned on the result so the caller provisions it after claiming.
    if pre_approve:
        admin_ws = WsRpcAdminClient(
            nest_url,
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            admin_ws.provision_tls_cert_blob(tls_blob)

    # ── 4. Operator-hatch with ephemeral bind addresses + caller extras.
    # DISTINCT ports (held simultaneously) — sequential find_free_port() can
    # collide and crash the second colliding listener with "address already in use".
    mx_port, submission_465_port, submission_587_port, metrics_port = find_free_ports(4)
    op_hatch_path = tmp / "operator-hatch.toml"
    op_hatch_path.write_text(
        f'data_dir = "{tmp.as_posix()}"\n'
        f'mta_bind_addr = "127.0.0.1:{mx_port}"\n'
        f'mta_bind_addr_465 = "127.0.0.1:{submission_465_port}"\n'
        f'mta_bind_addr_587 = "127.0.0.1:{submission_587_port}"\n'
        f'metrics_bind_addr = "127.0.0.1:{metrics_port}"\n'
        + operator_hatch_extra
    )

    # ── 5. Spawn the bridge.
    bridge_env = os.environ.copy()
    _apply_bridge_ffi_env(bridge_env)
    # Role-specific filename: the `dedicated_mail_nest` fixture passes the SAME
    # `tmp` to both `_spawn_mta_bridge` and `_spawn_mda_bridge`, so a shared
    # `mail-bridge.log` had the two processes truncate each other's log on
    # `open(..., "wb")` — leaving the MDA's CalDAV REPORT/decrypt diagnostics
    # unreadable. Distinct names keep both logs intact.
    log_file_path = tmp / "mail-bridge-mta.log"
    log_fh = open(log_file_path, "wb")
    # Captured so `respawn()` can relaunch the identical process after an
    # exit-for-rebind (the e2e stands in for s6) — same shape the MDA spawn
    # already used.
    spawn_argv = [
        mail_bridge_binary,
        f"--keypair-file={keyfile_path}",
        f"--nest-endpoint={nest_url}",
        f"--operator-hatch={op_hatch_path}",
        "--log-level=debug",
    ]
    from drivers.port_util import popen_group_kwargs, reap_descendants_of
    proc = subprocess.Popen(
        spawn_argv,
        stdout=log_fh,
        stderr=subprocess.STDOUT,
        env=bridge_env,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — `popen_group_kwargs()` is
    # `{}` there, so without this the bridge's only protection is the atexit
    # sweep a killed run never reaches (testing.md § point 9). No-op off Windows.
    reap_descendants_of(proc.pid)
    track_process(proc)

    # An unapproved bridge (pre_approve=False) sits in its zero-touch
    # `request_enrollment` poll and binds NO listeners until an admin approves
    # it, so there's nothing to wait for here — the caller waits for serving
    # after driving the approval (e.g. via the client UI).
    if wait_for_serving:
        try:
            _wait_for_mail_bridge_metrics(metrics_port, timeout=30.0)
            _wait_for_tcp_accept(mx_port, timeout=30.0)
        except Exception:
            # Spawn failed to come up — tear down the partial process so a failed
            # fixture/helper never leaks a bridge.
            try:
                if proc.poll() is None:
                    proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()
            except Exception:
                pass
            untrack_process(proc)
            log_fh.close()
            raise

    return _SpawnedMtaBridge(
        proc=proc,
        bridge_id=bridge_id,
        mx_port=mx_port,
        submission_port_465=submission_465_port,
        submission_port_587=submission_587_port,
        metrics_port=metrics_port,
        keyfile_path=str(keyfile_path),
        log_file_path=str(log_file_path),
        log_fh=log_fh,
        ed25519_pubkey=ed_pubkey,
        x25519_pubkey=x_pubkey,
        cert_pem=cert_pem,
        tls_blob=tls_blob,
        spawn_argv=spawn_argv,
        spawn_env=bridge_env,
    )


@pytest.fixture(scope="session")
def mail_bridge_mta(mail_bridge_binary, seal_helper_binary, nest_instance, tmp_path_factory):
    """Spawn fauna-mail-bridge in MTA role against `nest_instance`.

    This fixture owns the **nest-global** deployment state (provisioned once,
    reused read-only by any bridge spawned against this nest): the primary
    local domain, a recipient actor + its MLS pubkey + account alias, the
    (NOT the admin's key: the admin is shared — see § 4), the cleared DNS-dependent
    perimeter gates (`put_spam_policy`), a submission sender actor + alias +
    MLS pubkey + its wrapped submission token, and the in-process stub MXes /
    fake scanners. It then delegates the **per-bridge** spawn (keypair, admin
    enrollment, TLS seal, ephemeral ports, operator-hatch, process) to
    `_spawn_mta_bridge` and yields a handle the tests drive via raw SMTP.

    E.3 (tracked internally): the per-bridge TLS blob is sealed to the
    bridge's ephemeral X25519 pubkey + provisioned over Admin-class WS-RPC (in
    `_spawn_mta_bridge`), so the implicit-TLS submission listener (465) binds
    with a real cert and AUTH PLAIN succeeds. Outbound DATA is DKIM-signed by
    the nest at the hand-out, with the key it minted when the domain was
    added. The operator-hatch `[mta_mx_override]` routes `external.test`
    (and the primary domain, for the NDR path) at the stub MX so the
    submission round-trip needs no external DNS.

    The split is what lets `disposable_mta_bridge` stand up a *second* bridge
    on this same nest-global state for the destructive graceful-shutdown test
    without stranding this session-scoped one.

    Process safety: owns its spawned subprocess (via `_SpawnedMtaBridge`),
    tracks it through drivers/port_util.track_process atexit cleanup, never
    `pkill`/`killall` (see README.md § Ground rules — killing by name can hit
    processes owned by other work sharing the machine).
    """
    import time as _time
    import base64 as _b64

    tmp = tmp_path_factory.mktemp("mail-bridge-mta")
    domain = MAIL_PRIMARY_DOMAIN
    # A second non-primary local domain with its own DKIM key, for the
    # per-domain From-domain DKIM-selection tier_3 test. Claimed below, which
    # is what mints its nest-held key. The selector is
    # "default" (the conventional first selector): a row's `dkim_selector` is
    # NULL until a rotation sets it, so nest projects "default" for every
    # domain — the per-domain distinction proven here is the KEY (a distinct
    # `default._domainkey.second.test` record), not the selector label.
    second_domain = MAIL_SECOND_DOMAIN
    second_dkim_selector = "default"
    recipient_local_part = "recipient"

    nest_port = nest_instance["port"]
    nest_url = nest_instance["url"]

    # ── Nest-global deployment state (provisioned once, reused read-only by
    # every bridge spawned against this nest — keypair/enrollment/TLS are
    # per-bridge and live in `_spawn_mta_bridge`).

    # The primary local domain (mail_domains row) — read by the bridge's
    # `fetch_config` projection for the RCPT-TO local-domains list and the EHLO /
    # TLS / DKIM primary anchor (mail-multidomain.md § The primary domain) — is
    # the autouse `_session_primary_mail_domain` fixture's `MAIL_PRIMARY_DOMAIN`
    # claim. The `add_local_domain` call below is therefore an idempotent
    # `skipped: true` re-add (same `domain` value) and serves only to surface
    # the row in the bridge's local-domains snapshot via the existing
    # config-fetch path.

    # ── 4. Provision the recipient actor + its alias + MLS pubkey via
    # the production WS-RPC paths — no SQLite hand-pokes.
    #
    # The mail-bridge sealing review landed the writers:
    #   - the member's own `fauna.bridges.create_account_alias`
    #     (`helpers.mail_aliases.add_exact_alias`) writes the `account_aliases`
    #     (kind='exact') row that `validate_recipient` resolves through;
    #     the old standalone `recipient_routes` table was dropped (#1).
    #   - `fauna.bridges.provision_recipient_mls_pubkey` writes the
    #     `actor_mls_pubkeys` row the MTA HPKE-seals `encrypted_body` to
    #     on every inbound DATA (#2).
    # The seal key is derived from a throwaway MSEK
    # (`helpers/recipient_seal_key.py`) — the bridge seals to it; the test
    # asserts the round-trip succeeded but never decrypts, so the MSEK is
    # dropped.
    from common.auth import create_actor_and_register as _create_actor
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    # Deferred: the helper's client imports nacl at module level, and the
    # shipped fakes suite loads this conftest without nacl installed.
    from helpers.mail_aliases import add_exact_alias
    from helpers.recipient_seal_key import provision_recipient_seal_key

    def _seal(subcommand, params):
        return _run_seal_helper(seal_helper_binary, subcommand, params)

    recipient = _create_actor(
        nest_port,
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    recipient_actor_id = recipient["actor_id_bytes"]

    # Role-address mail (postmaster@/abuse@/noc@/security@ per
    # smtp-server.md § abuse@/postmaster@ routing) resolves to the
    # deployment admin's mailbox, and this nest's admin is SHARED with every
    # `admin_app` test, so no key is written for it here: a harness key is
    # one the admin's app never holds (e2e-conventions.md convention 10,
    # *A sixth form*; `helpers/shared_identity.py` refuses the write). The
    # role-address journeys run on `dedicated_mail_nest`, whose admin is theirs.

    # ── Submission identity (nest-global). The sender authenticates
    # submission; its alias lets validate_recipient resolve the AUTH
    # username, and its Ed25519 identity key signs the submission token. The
    # token is sealed under the MUA credential keyed on the sender's actor_id
    # (no bridge pubkey), so it's nest-global like the actors above — unlike
    # the per-bridge TLS blob that `_spawn_mta_bridge` seals to each bridge's
    # own keypair.
    submission_credential = b"correct horse battery staple"
    submission_sender_local = "sender"
    # A SECOND address the SAME sender actor owns, whose local-part differs
    # from the login handle. Lets a submission test drive MAIL FROM an *owned
    # alias* (not the login-handle fast-path), exercising the real
    # `assertMailFromOwned` → resolve_recipient → actor_id-match acceptance
    # branch end-to-end (mail-multidomain.md § Cross-domain submission policy).
    # macOS Mail's "From" selector sets exactly this kind of owned-alias sender.
    submission_sender_owned_alias_local = "sender.alias"
    _now = int(_time.time())

    # Sender actor: the authenticated submission identity. Its alias lets
    # validate_recipient resolve the AUTH username; its Ed25519 identity
    # key signs the submission token (actor_id == verifying key).
    sender = _create_actor(
        nest_port,
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    sender_actor_id = sender["actor_id_bytes"]
    sender_seed = bytes(sender["signing_key"])
    # The sender's own "Sent" copy is HPKE-sealed to the sender's MLS
    # pubkey (dispatchFaunaRecipients always submits it), so the sender —
    # like any Fauna actor that receives mail — needs a seal key on file.
    token_blob = _run_seal_helper(seal_helper_binary, "seal-submission-token", {
        "signing_seed_b64": _b64.b64encode(sender_seed).decode(),
        "credential_id": "default",
        "credential_kind": "plain",
        "credential_b64": _b64.b64encode(submission_credential).decode(),
        "issued_at": _now,
        "expires_at": _now + 86400,
        "max_recipients": 100,
        "max_messages_per_day": 1000,
    })

    admin = nest_instance["admin"]
    admin_ws = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        # Stage-5 default-off seed: an unset `mail_enabled` toggle reads OFF
        # (`mail-bridge-lifecycle.md` § Default-off on first claim), and the MTA
        # gates its listeners on the snapshot's `mail_enabled` — so the fixture
        # enables mail explicitly BEFORE the bridge's first `fetch_config`,
        # mirroring production (the claim-time § 3b glue / admin-mail toggle).
        admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        # Claim the primary local domain over WS-RPC (no-HTTP directive).
        # `is_primary` is nest-derived (first active domain = primary), so it
        # is not a request field; idempotent on domain_name. Must precede the
        # alias calls below, which resolve through `local_domain=domain`.
        admin_ws.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": domain,
                "mta_sts_cert_mode": "per_host",
            },
        )
        # Claim the SECOND (non-primary) local domain so it appears in the
        # bridge's startup local-domains + DkimSelectors projection. Adding a
        # mail domain mints its own DKIM key nest-side (read back below).
        # Non-primary (the primary is claimed first), so it never disturbs the
        # primary-domain behaviour the other tests assert.
        admin_ws.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": second_domain,
                "mta_sts_cert_mode": "per_host",
            },
        )
        add_exact_alias(nest_url, recipient["signing_key"], domain, recipient_local_part)
        provision_recipient_seal_key(
            admin_ws, recipient_actor_id, run_seal_helper=_seal,
        )
        # Clear the DNS-dependent perimeter gates via the production
        # admin override (A3 Bucket B's `put_spam_policy`), not a CLI/env
        # hack — so the test stays tier_3 (real nest, real bridge, real
        # wire) yet needs no external DNS:
        #   - `dnsbl_servers = []`  — no DNSBL queries (so port 25 can
        #     bind 127.0.0.1; zen.spamhaus.org returns 127.255.255.254
        #     for 127.0.0.x and would otherwise reject at CONNECT).
        #   - `greylist_enabled = false` — first RCPT is accepted, so the
        #     test is a single connection (no first-seen tempfail dance).
        #   - `fcrdns_mode = "off"` — skips the connection-time PTR +
        #     forward-A lookup in `NewSession` (`server.go:196`). That
        #     lookup is the documented `test_inbound_mx_round_trip` flake:
        #     it resolves sub-second in an interactive shell but can hang
        #     >30 s under a pytest-spawned subprocess. With it off the
        #     bridge does no resolver call on the loopback peer.
        #   - `max_conn_per_min = 1000` — every test in this session-scoped
        #     suite connects from 127.0.0.1, so they share one per-IP
        #     connection-rate budget (`server.go` RateLimiter, default ~10-25
        #     /min). As the file grows, a burst of back-to-back tests trips
        #     the limiter and a downstream test gets a spurious `421`. Lift it
        #     out of the way (the limiter itself is asserted by
        #     `api/test_smtp_policy_enforcement.py` against its own fixture).
        admin_ws.call(
            "fauna.bridges.put_spam_policy",
            {
                "dnsbl_servers": [],
                "greylist_enabled": False,
                "greylist_delay_secs": 0,
                "fcrdns_mode": "off",
                "max_conn_per_min": 1000,
                "baseline_standing_publish": False,
            },
        )
        # Sender alias so the AUTH username `sender@domain` resolves via
        # validate_recipient (same account_aliases path as the recipient).
        add_exact_alias(nest_url, sender["signing_key"], domain, submission_sender_local)
        # A SECOND alias the SAME sender actor owns, local-part ≠ login handle.
        # `assertMailFromOwned` resolves MAIL FROM:<this@domain> back to the
        # sender's actor_id and accepts it — the resolver-backed owned-alias
        # submission path the login-handle fast-path skips. No separate MLS
        # pubkey needed: the actor (and thus its Sent-copy seal target) is the
        # same sender_actor_id provisioned below.
        add_exact_alias(
            nest_url, sender["signing_key"], domain, submission_sender_owned_alias_local
        )
        # Sender's seal key — the bridge seals the sender's "Sent" copy
        # to it on submission (550 5.1.1 otherwise).
        provision_recipient_seal_key(
            admin_ws, sender_actor_id, run_seal_helper=_seal,
        )

    # The submission token is User-class (nest keys it on the *caller's*
    # actor_id), so provision it over a WS-RPC connection authenticated as
    # the sender, not the admin.
    sender_ws = WsRpcAdminClient(
        nest_url,
        actor_id=sender_actor_id,
        signing_key=sender_seed,
    )
    with sender_ws:
        sender_ws.provision_wrapped_submission_token(token_blob)

    # Stub external MX for the submission round-trip's outbound delivery.
    # Started before the operator-hatch is written so its ephemeral port
    # can be wired into mta_mx_override.
    from helpers.stub_mx import StubMX
    stub_mx = StubMX().start()
    # A second, TLS-capable stub MX on a distinct loopback IP (127.0.0.2) for
    # the outbound DANE/TLSA tests (T2.1b). It advertises + serves STARTTLS
    # with a self-signed cert whose DER the test hashes into the published
    # TLSA record. The distinct bind IP keeps its matcher host (`127.0.0.2`)
    # separate from the plaintext stub's `127.0.0.1`, so a DANE TLSA override
    # (keyed on MX host) can't contaminate the plaintext / MTA-STS tests that
    # route through `external.test` → 127.0.0.1.
    #
    # macOS only makes 127.0.0.1 a loopback address by default (Linux/Windows
    # route the whole 127.0.0.0/8), so binding 127.0.0.2 raises EADDRNOTAVAIL on
    # a bare mac host — which would break the WHOLE mail fixture for every macos
    # mail test, not just the DANE ones. Fall back to a distinct ephemeral port
    # on 127.0.0.1: the DANE override + TLSA pin are keyed on the MX *host*
    # (`dane.test`) and the presented cert, not the bind IP, so routing and
    # validation are unaffected; the distinct-IP isolation is belt-and-suspenders
    # the Linux/CI DANE suite keeps (no macos-marked test drives the DANE path).
    try:
        tls_stub_mx = StubMX(bind_host="127.0.0.2", enable_starttls=True).start()
    except OSError:
        tls_stub_mx = StubMX(bind_host="127.0.0.1", enable_starttls=True).start()

    # Fake clamd + rspamd for the T1.4 content-scan gate. Started before the operator-hatch is written so their
    # ephemeral ports wire into `clamd_addr` / `rspamd_url`. They behave like
    # real co-located daemons — clamd scans clean unless the body carries the
    # infection marker, rspamd returns a benign score — so the existing
    # inbound round-trip (a benign message) still delivers, and the new scan
    # tests drive the reject path with an infected body. In-process daemon
    # threads, same reliability tier as the stub MX above.
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd
    fake_clamd = FakeClamd().start()
    fake_rspamd = FakeRspamd().start()

    # ── Spawn the session bridge. The operator-hatch extras wire the
    # scanners and the `[mta_mx_override]` routes onto it:
    #   - `clamd_addr` / `rspamd_url` → the fake scanners (T1.4 scan gate).
    #   - `external.test` → the plaintext stub (submission round-trip).
    #   - the primary `domain` → the same stub, so the outbound NDR path
    #     (T1.2) is observable: a permanent-failure bounce enqueued
    #     null-sender back to `<local>@<primary_domain>` MX-delivers there.
    #     (Local recipients resolve at RCPT TO via the MDA, so this route is
    #     inert for the existing inbound tests.)
    #   - `dane.test` → the TLS-capable stub (outbound DANE tests).
    op_hatch_extra = (
        f'clamd_addr = "{fake_clamd.addr}"\n'
        f'rspamd_url = "{fake_rspamd.url}"\n'
        f'\n[mta_mx_override]\n'
        f'"external.test" = "{stub_mx.target}"\n'
        f'"{domain}" = "{stub_mx.target}"\n'
        f'"dane.test" = "{tls_stub_mx.target}"\n'
    )
    spawned = _spawn_mta_bridge(
        mail_bridge_binary=mail_bridge_binary,
        seal_helper_binary=seal_helper_binary,
        nest_instance=nest_instance,
        tmp=tmp,
        bridge_id="test-mta-1",
        domain=domain,
        operator_hatch_extra=op_hatch_extra,
    )

    try:
        # DKIM: the nest holds each mail domain's key and signs at the outbound
        # hand-out, so the fixture seals nothing to the bridge. It reads back
        # the record the nest publishes for each domain for the tests' dkimpy
        # `dnsfunc`.
        dkim_selector = "default"
        published_dkim = {
            dkim_domain: _published_dkim_record(nest_instance, dkim_domain, selector)
            for dkim_domain, selector in (
                (domain, dkim_selector),
                (second_domain, second_dkim_selector),
            )
        }

        handle = MailBridgeMTAHandle(
            proc=spawned.proc,
            mx_port=spawned.mx_port,
            submission_port_465=spawned.submission_port_465,
            submission_port_587=spawned.submission_port_587,
            metrics_port=spawned.metrics_port,
            domain=domain,
            bridge_id=spawned.bridge_id,
            bridge_role="mta",
            recipient_local_part=recipient_local_part,
            recipient_actor=recipient,
            keypair_file=spawned.keyfile_path,
            log_file=spawned.log_file_path,
            tls_trust_anchor=spawned.cert_pem,
            ed25519_pubkey=spawned.ed25519_pubkey,
            x25519_pubkey=spawned.x25519_pubkey,
            submission_credential=submission_credential,
            submission_sender_local=submission_sender_local,
            submission_sender_owned_alias_local=submission_sender_owned_alias_local,
            submission_sender_actor=sender,
            stub_mx=stub_mx,
            nest_url=nest_url,
            tls_stub_mx=tls_stub_mx,
            dane_domain="dane.test",
            dkim_selector=dkim_selector,
            dkim_public_dns_value=published_dkim[domain],
            second_domain=second_domain,
            second_dkim_selector=second_dkim_selector,
            second_dkim_public_dns_value=published_dkim[second_domain],
        )
        yield handle
    finally:
        spawned.cleanup()
        stub_mx.stop()
        tls_stub_mx.stop()
        fake_clamd.stop()
        fake_rspamd.stop()


@pytest.fixture
def disposable_mta_bridge(
    mail_bridge_mta, mail_bridge_binary, seal_helper_binary, nest_instance,
    tmp_path_factory,
):
    """A throwaway MTA bridge for *destructive* tests (e.g. the graceful-
    shutdown SIGTERM drain), so a test can kill a bridge without stranding
    the session-scoped one every other test in the file shares.

    Depending on `mail_bridge_mta` guarantees the nest-global deployment
    state (primary local domain, recipient/sender actors + aliases + MLS
    pubkeys, spam policy) is already provisioned; this fixture only adds a
    *second* bridge process with its own keypair + enrollment + ephemeral
    ports, reusing that state read-only. The unique `bridge_id` gives it a
    distinct nest WS actor (two bridges on one keypair would collide) and a
    distinct `(role, bridge_id, domain)` TLS-blob key, so neither the
    enrollment nor the TLS provision clobbers the session bridge.

    No scanners / `[mta_mx_override]` are wired: a drain test never reaches
    DATA or outbound delivery. The session handle is reused only for its
    read-only nest-global fields (domain, recipient, submission identity).
    """
    session = mail_bridge_mta
    tmp = tmp_path_factory.mktemp("mail-bridge-mta-disposable")
    spawned = _spawn_mta_bridge(
        mail_bridge_binary=mail_bridge_binary,
        seal_helper_binary=seal_helper_binary,
        nest_instance=nest_instance,
        tmp=tmp,
        bridge_id=f"test-mta-disposable-{uuid.uuid4().hex[:12]}",
        domain=session.domain,
        operator_hatch_extra="",
    )
    try:
        handle = MailBridgeMTAHandle(
            proc=spawned.proc,
            mx_port=spawned.mx_port,
            submission_port_465=spawned.submission_port_465,
            submission_port_587=spawned.submission_port_587,
            metrics_port=spawned.metrics_port,
            domain=session.domain,
            bridge_id=spawned.bridge_id,
            bridge_role="mta",
            recipient_local_part=session.recipient_local_part,
            recipient_actor=session.recipient_actor,
            keypair_file=spawned.keyfile_path,
            log_file=spawned.log_file_path,
            tls_trust_anchor=spawned.cert_pem,
            ed25519_pubkey=spawned.ed25519_pubkey,
            x25519_pubkey=spawned.x25519_pubkey,
            submission_credential=session.submission_credential,
            submission_sender_local=session.submission_sender_local,
            stub_mx=session.stub_mx,
            nest_url=session.nest_url,
            tls_stub_mx=session.tls_stub_mx,
            dane_domain=session.dane_domain,
        )
        yield handle
    finally:
        spawned.cleanup()


class MailBridgeMDAHandle:
    """Handle returned by the `mail_bridge_mda` fixture.

    Mirrors `MailBridgeMTAHandle` but exposes the MDA-role listener
    ports (IMAPS, IMAP/STARTTLS, CalDAV-over-HTTPS) instead of the
    MTA ones. Unlike the MTA, the MDA role hard-gates on
    `TLSProvider != nil` (`bins/fauna-bridges/internal/mda/
    mda.go:129-131`), so the fixture must provision a real TLS cert
    before spawning the binary — that's what the
    `fauna.bridges.provision_self_signed_cert` WS-RPC step below
    exercises.
    """

    def __init__(
        self,
        proc,
        imaps_port,
        imap_starttls_port,
        caldav_port,
        metrics_port,
        domain,
        bridge_id,
        bridge_role,
        recipient_actor,
        recipient_username,
        recipient_password,
        keypair_file,
        log_file,
        tls_trust_anchor,
        ed25519_pubkey,
        x25519_pubkey,
        x25519_secret,
    ):
        self.proc = proc
        self.imaps_port = imaps_port
        self.imap_starttls_port = imap_starttls_port
        self.caldav_port = caldav_port
        self.metrics_port = metrics_port
        self.domain = domain
        self.bridge_id = bridge_id
        self.bridge_role = bridge_role
        self.recipient_actor = recipient_actor
        # IMAP/CalDAV MUA-AUTH credentials: AUTH PLAIN with
        # (recipient_username, recipient_password); the bridge AEAD-unwraps
        # the wrapped-MSEK blob the fixture provisioned (tracked internally).
        self.recipient_username = recipient_username
        self.recipient_password = recipient_password
        self.keypair_file = keypair_file
        self.log_file = log_file
        self.tls_trust_anchor = tls_trust_anchor
        self.ed25519_pubkey = ed25519_pubkey
        self.x25519_pubkey = x25519_pubkey
        # The bridge's x25519 secret is exposed on the handle so tests
        # that pre-stage user-side wrapped blobs (Task D follow-up after
        # the sealing review's seal-helper fix lands) can seal directly
        # to the bridge if a test needs the bridge to unseal data the
        # admin-uploaded path would normally seal.
        self.x25519_secret = x25519_secret

    def kill(self):
        try:
            self.proc.terminate()
        except Exception:
            return
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


class _SpawnedMdaBridge(_SpawnedBridge):
    """The per-bridge mechanical result of `_spawn_mda_bridge`.

    Holds only the fields unique to one MDA bridge process — its enrolled
    keypair, its ephemeral IMAPS / IMAP-STARTTLS / CalDAV / metrics ports, the
    spawned process + log handle — so a caller can assemble a
    `MailBridgeMDAHandle` around it (mixing in the nest-global state it owns)
    and `cleanup()` the process independently of any other bridge.

    The MDA's TLS cert is *domain*-scoped, not per-bridge: `_spawn_mda_bridge`
    calls `provision_self_signed_cert` after hand-poking this bridge's x25519
    so the fan-out reaches it, but the cert is shared across every approved
    bridge on
    the domain (unlike the MTA's per-`(role, id, domain)` sealed blob).
    """

    def __init__(
        self,
        *,
        proc,
        bridge_id,
        imaps_port,
        imap_starttls_port,
        caldav_port,
        metrics_port,
        keyfile_path,
        log_file_path,
        log_fh,
        ed25519_pubkey,
        x25519_pubkey,
        x25519_secret,
        spawn_argv=None,
        spawn_env=None,
    ):
        self._init_process(
            proc=proc,
            log_file_path=log_file_path,
            log_fh=log_fh,
            metrics_port=metrics_port,
            spawn_argv=spawn_argv,
            spawn_env=spawn_env,
        )
        self.bridge_id = bridge_id
        self.imaps_port = imaps_port
        self.imap_starttls_port = imap_starttls_port
        # The CalDAV-HTTPS listener port. When the bridge was spawned with the
        # `caldav_listen_https` operator-hatch (`pin_caldav_hatch=True`, the
        # default) this is the ephemeral hatch port the listener binds. When
        # spawned WITHOUT it (`pin_caldav_hatch=False`) this is `None` — the
        # listener binds the admin-set `caldav_port` from nest config instead,
        # which only the caller knows, so reading this would be a bug.
        self.caldav_port = caldav_port
        self.keyfile_path = keyfile_path
        self.ed25519_pubkey = ed25519_pubkey
        self.x25519_pubkey = x25519_pubkey
        self.x25519_secret = x25519_secret

    # respawn() / rebind() / cleanup() are the shared _SpawnedBridge machinery.
    # "Serving" for an MDA is the base's metrics wait: which listeners it binds
    # depends on which protocols the snapshot enables, so there is no single
    # port to anchor on — the admin-CalDAV-port rebind test asserts its own new
    # port itself.


def _spawn_mda_bridge(
    *,
    mail_bridge_binary,
    nest_instance,
    tmp,
    bridge_id,
    domain,
    bridge_role="mda",
    pre_approve=True,
    wait_for_serving=True,
    pin_caldav_hatch=True,
    operator_hatch_extra="",
):
    """Spawn one MDA-role `fauna-mail-bridge` process against `nest_instance`.

    `operator_hatch_extra` (default `""`) is appended verbatim to the operator-
    hatch TOML — the MDA counterpart of `_spawn_mta_bridge`'s param. The re-score
    drain test uses it to wire the co-resident `clamd_addr` / `rspamd_url` the
    drain re-runs scans against (mirrors the MTA fixture's fake-scanner block);
    inert for every other MDA test (the drain only fires when a capability holder
    has live grants).

    Owns *only* the per-bridge work two bridges can never share: a fresh
    service-user keypair + its `register_user` + admin enrollment
    (so the bridge's nest WS actor is unique), the x25519 SQLite hand-poke
    (production fix tracked in the `mail_bridge_mda` docstring),
    a `fauna.bridges.provision_self_signed_cert` WS-RPC call that fans the
    domain cert out to *this* bridge's x25519, the four ephemeral bind ports,
    the operator-hatch, and the spawned
    subprocess.

    Nest-global deployment state — the claimed local domain, the
    pre-provisioned recipient actor — is the *caller's* responsibility
    (provisioned once, reused read-only across bridges). That
    separation is what lets the destructive graceful-shutdown test stand up its
    own disposable bridge and SIGTERM it without stranding the session-scoped
    one.

    `self_signed_cert` is domain-scoped: it re-synthesizes the cert and seals
    it to *every* approved bridge with an x25519 on the domain. Re-POSTing from
    a second (disposable) bridge is harmless to the already-running session
    bridge (which fetched its cert once at startup); it only guarantees the new
    bridge's startup TLS-cert fetch finds a blob sealed to its own x25519. The
    domain must already be claimed by the caller (the kind returns
    `not_found` otherwise).

    `pre_approve=False` (mirroring `_spawn_mta_bridge`) spawns the bridge against
    an UNCLAIMED nest — `nest_instance["admin"]` is `None`, so the four
    admin-dependent steps (register_user, the `request_enrollment` +
    `approve_pending_bridge` pre-approve, the x25519 SQLite hand-poke, and the Admin-WS-RPC
    `provision_self_signed_cert`) are ALL skipped. The bridge then cold-boots
    into its zero-touch `request_enrollment` poll (a PENDING row) and binds no
    listeners until an admin approves it later — e.g. through the client's
    admin-bridges-pending UI after the client claims the nest. `wait_for_serving`
    (default `True`) waits for `/healthz`; pass `False` with `pre_approve=False`
    so the caller doesn't block on a listener that won't bind until approval.

    `pin_caldav_hatch` (default `True`) writes the `caldav_listen_https`
    operator-hatch line so the CalDAV listener binds an ephemeral loopback port
    the caller controls (the canonical tier_3 shape — every existing fixture).
    Pass `False` to OMIT that line so the listener binds the admin-set
    `caldav_port` from nest config instead (`resolveMDAListenAddrs` falls back to
    `:<EffectiveCalDAVPort>` when no hatch, and `CalDAVBindIsAdminPort` then arms
    the rebind-on-`config_changed` exit) — the shape the admin-CalDAV-port rebind
    acceptance test needs, where the admin `caldav_port` (not a fixed hatch) is
    authoritative and `respawn()` re-binds it after a port change. With `False`
    the returned `caldav_port` is `None` (the bound port is the caller's admin
    value, unknown here).

    Returns a `_SpawnedMdaBridge`; the caller wraps it in a
    `MailBridgeMDAHandle` and calls `.cleanup()` on teardown.
    """
    import time as _time
    import sqlite3
    from nacl.signing import SigningKey as NaClSigningKey
    from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
    from cryptography.hazmat.primitives.serialization import (
        Encoding, NoEncryption, PrivateFormat, PublicFormat,
    )
    import cbor2
    from drivers.port_util import find_free_port, find_free_ports, track_process, untrack_process
    from common.auth import register_user as _register_user
    from helpers.bridge_enrollment import enroll_and_approve_bridge

    nest_port = nest_instance["port"]
    nest_url = nest_instance["url"]
    # `admin` is None on an unclaimed nest (pre_approve=False) — every
    # admin-dependent step below (register_user, the pre-approve, the
    # x25519 SQLite poke, the WS-RPC cert provision) is gated on `pre_approve`.
    admin = nest_instance["admin"]

    # ── 1. Service-user keypair (Ed25519 signing + X25519 wrap-target).
    ed_sk = NaClSigningKey.generate()
    ed_seed = bytes(ed_sk)
    ed_pubkey = bytes(ed_sk.verify_key)

    x_sk = X25519PrivateKey.generate()
    x_priv = x_sk.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption())
    x_pubkey = x_sk.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)

    # CBOR keyfile — mirrors libs/fauna-mls/src/wrapped_blob/
    # service_user.rs::ServiceUserKeyfile + the Go decoder at
    # bins/fauna-bridges/internal/keypair/keyfile.go. `canonical=True` →
    # DAG-CBOR length-first key order (the Go decoder rejects non-canonical
    # maps; CBOR-DAG-everywhere refactor).
    keyfile_bytes = cbor2.dumps({
        "v": 1,
        "role": bridge_role,
        "bridge_id": bridge_id,
        "ed25519_seed": ed_seed,
        "x25519_priv": x_priv,
        "created_at": int(_time.time()),
    }, canonical=True)
    # Name the keyfile `<role>.key` (mta.key / mda.key) to mirror production
    # (docker/s6 `/data/keys/{mta,mda}.key`): the bridge's zero-touch
    # self-enrollment derives its `role_hint` from the keyfile *basename*
    # (`roleHintFromKeyfile` → `request_enrollment`), and nest rejects any
    # role_hint that isn't "mta"/"mda" (`bridge_blob_handlers.rs`). A generic
    # name like `mail-bridge.key` would self-enroll as role_hint="mail-bridge"
    # → ok=false → the bridge exits before binding.
    keyfile_path = tmp / f"{bridge_role}.key"
    keyfile_path.write_bytes(keyfile_bytes)
    keyfile_path.chmod(0o600)

    # ── 2-4. Admin-side enrollment, x25519 hand-poke, and cert provisioning.
    # ALL gated on `pre_approve` (claimed nest). With `pre_approve=False` the
    # nest is unclaimed (`admin is None`) and these steps are skipped: the bridge
    # cold-boots into its zero-touch `request_enrollment` poll (a PENDING row)
    # and an admin approves it later — e.g. via the client's admin-bridges-pending
    # UI after the client claims the nest. (The MTA spawn gates its pre-approve
    # the same way.)
    if pre_approve:
        # ── 2. Workaround: register the bridge as a user so the
        # `fauna.auth.verify` WS-RPC kind resolves. Same pattern as
        # `_spawn_mta_bridge`. register_user takes only (port, actor_id)
        # positionally — credentials are keyword-only (admin_signing_key); the
        # legacy positional admin_token was ripped with the admin HTTP twins
        # (common/auth.py register_user docstring).
        _register_user(
            nest_port, ed_pubkey.hex(),
            admin_signing_key=admin["signing_key"],
            base_url=nest_url,  # serve_tls nest → WS-RPC over the https base
        )
        enroll_and_approve_bridge(
            nest_url, admin["signing_key"], ed_pubkey, bridge_role, bridge_id,
        )

        # ── 3. SQLite hand-poke bridge_service_users.x25519_pubkey BEFORE cert
        # provisioning so the self_signed_cert seal fans out to this bridge. The
        # production-path alternatives are in the `mail_bridge_mda` docstring;
        # tracked internally the same way the MTA fixture's
        # recipient_routes hand-poke is tracked internally in the
        # mail-bridge sealing review.
        conn = sqlite3.connect(nest_instance["db_path"], timeout=10.0)
        try:
            conn.execute(
                "UPDATE bridge_service_users SET x25519_pubkey = ?"
                " WHERE ed25519_pubkey = ? AND status != 'revoked'",
                (x_pubkey, ed_pubkey),
            )
            conn.commit()
        finally:
            conn.close()

        # ── 4. Admin provisions the self-signed cert over WS-RPC (Task B,
        # tracked internally; no-HTTP directive). Nest synthesizes the cert via
        # rcgen and seals it to every approved bridge with an x25519 (incl. the
        # one just hand-poked), then writes the on-disk PEM. The bridge's startup
        # TLS-cert fetch will succeed against the blob this call stores.
        from clients.ws_rpc_admin_client import WsRpcAdminClient as _WsRpcAdminClient
        _cert_ws = _WsRpcAdminClient(
            nest_url,
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        # `domain=None` is a domainless home nest: there is no domain to issue a
        # cert for, and the MDA serves the nest's self-signed FLOOR cert instead
        # (caldav-server.md § Any-locator serving), so nothing is provisioned.
        cert_reply = {"bridges_sealed_to": [{"role": bridge_role, "bridge_id": bridge_id}]}
        if domain is not None:
            with _cert_ws:
                cert_reply = _cert_ws.call(
                    "fauna.bridges.provision_self_signed_cert",
                    {"domain": domain, "additional_dns_sans": []},
                )
        # Sanity-check that our bridge is in the sealed-to set — if not, the seal
        # silently skipped (e.g. the hand-poke above didn't land), which would
        # surface only as a later /healthz timeout that's painful to diagnose.
        sealed_roles_ids = {
            (b["role"], b["bridge_id"])
            for b in cert_reply.get("bridges_sealed_to", [])
        }
        if (bridge_role, bridge_id) not in sealed_roles_ids:
            raise AssertionError(
                f"self_signed_cert reply did not include ({bridge_role}, "
                f"{bridge_id}) in bridges_sealed_to — fixture x25519 "
                f"hand-poke may have failed; reply was {cert_reply!r}"
            )

    # ── 5. Operator-hatch TOML with ephemeral loopback bind addresses for
    # IMAPS / IMAP+STARTTLS / CalDAV-HTTPS + metrics. DISTINCT ports (one
    # allocation holding all sockets at once) — sequential `find_free_port()`
    # calls can collide (imaps==caldav was observed), crashing the second
    # listener to bind with "address already in use" and taking the MDA down.
    imaps_port, imap_starttls_port, caldav_port, metrics_port = find_free_ports(4)
    op_hatch_path = tmp / "operator-hatch.toml"
    # The `caldav_listen_https` line is OMITTED when `pin_caldav_hatch=False` so
    # the CalDAV listener binds the admin-set `caldav_port` from nest config (the
    # admin-port rebind test) instead of this fixed hatch port. IMAP + metrics
    # stay pinned either way (only CalDAV is under test).
    caldav_hatch_line = (
        f'caldav_listen_https = "127.0.0.1:{caldav_port}"\n' if pin_caldav_hatch else ""
    )
    op_hatch_path.write_text(
        f'data_dir = "{tmp.as_posix()}"\n'
        f'imap_listen_implicit_tls = "127.0.0.1:{imaps_port}"\n'
        f'imap_listen_starttls = "127.0.0.1:{imap_starttls_port}"\n'
        f'{caldav_hatch_line}'
        f'metrics_bind_addr = "127.0.0.1:{metrics_port}"\n'
        f'{operator_hatch_extra}'
    )

    # ── 6. Spawn the bridge.
    bridge_env = os.environ.copy()
    _apply_bridge_ffi_env(bridge_env)
    # Role-specific filename — see the MTA spawn's note: a shared
    # `mail-bridge.log` was truncated by whichever of the co-located MTA/MDA
    # processes opened it second, hiding the MDA's CalDAV logs.
    log_file_path = tmp / "mail-bridge-mda.log"
    log_fh = open(log_file_path, "wb")
    # Captured on the handle so `respawn()` re-launches the IDENTICAL process
    # (same keyfile + operator-hatch) after a config-driven rebind exit.
    spawn_argv = [
        mail_bridge_binary,
        f"--keypair-file={keyfile_path}",
        f"--nest-endpoint={nest_url}",
        f"--operator-hatch={op_hatch_path}",
        "--log-level=debug",
    ]
    from drivers.port_util import popen_group_kwargs, reap_descendants_of
    proc = subprocess.Popen(
        spawn_argv,
        stdout=log_fh,
        stderr=subprocess.STDOUT,
        env=bridge_env,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee (testing.md § point 9).
    reap_descendants_of(proc.pid)
    track_process(proc)

    # An unapproved bridge (pre_approve=False) sits in its zero-touch
    # `request_enrollment` poll and binds NO listeners (incl. the metrics mux's
    # role dispatch) until an admin approves it, so there's nothing to wait for
    # here — the caller waits for serving after driving the approval (e.g. via
    # the client UI). Mirrors `_spawn_mta_bridge`'s `wait_for_serving` gate.
    if wait_for_serving:
        try:
            _wait_for_mail_bridge_metrics(metrics_port, timeout=30.0)
        except Exception:
            # Spawn failed to come up — tear down the partial process so a failed
            # fixture/helper never leaks a bridge.
            try:
                if proc.poll() is None:
                    proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()
            except Exception:
                pass
            untrack_process(proc)
            log_fh.close()
            raise

    return _SpawnedMdaBridge(
        proc=proc,
        bridge_id=bridge_id,
        imaps_port=imaps_port,
        imap_starttls_port=imap_starttls_port,
        # `None` when the CalDAV hatch was omitted — the listener binds the
        # admin-set port the caller knows, not this (unwritten) hatch port.
        caldav_port=caldav_port if pin_caldav_hatch else None,
        metrics_port=metrics_port,
        keyfile_path=str(keyfile_path),
        log_file_path=str(log_file_path),
        log_fh=log_fh,
        ed25519_pubkey=ed_pubkey,
        x25519_pubkey=x_pubkey,
        x25519_secret=x_priv,
        spawn_argv=spawn_argv,
        spawn_env=bridge_env,
    )


class _MsekRecipient:
    """A mail recipient provisioned with MSEK-derived credentials.

    The result of `_provision_msek_recipient`: the created actor (the
    `create_actor_and_register` dict), its 32-byte actor_id, the
    `<local>@<domain>` username + MUA-AUTH password, and the raw MSEK both
    halves of the body-decrypt path were derived from (kept for tests that
    need to seal/open test-side).
    """

    def __init__(self, *, recipient, actor_id, username, password, msek):
        self.recipient = recipient
        self.actor_id = actor_id
        self.username = username
        self.password = password
        self.msek = msek


def _provision_msek_recipient(
    *, nest_instance, seal_helper_binary, domain: str | None, local_part: str, password: str,
    actor: dict | None = None,
) -> "_MsekRecipient":
    """Pre-provision a recipient actor + the credentials its IMAP/CalDAV MUA-AUTH
    *and body decrypt* need (tracked internally), all keyed off one MSEK so the
    seal/open halves match the production wiring:

      - an exact alias so `validate_recipient(local, domain)` resolves it,
      - the MSEK-derived recipient MLS pubkey (the inbound MTA / IMAP APPEND /
        CalDAV PUT seal target; AUTH fetches it),
      - a wrapped-MSEK blob sealed under `password` — the bridge AEAD-unwraps it
        at AUTH (`imap/auth.go` finishAuth → `UnwrapMsekBlob`), so unwrap-success
        IS the auth signal,
      - an MLS snapshot carrying the MSEK-derived leaf init keypair — the MDA
        `cap.Decrypt`s it at AUTH and opens bodies with its leaf secret
        (`imap/fetch.go`, `caldav/report.go` → `MlsCapability.OpenMailRecord`).

    Both halves of the body-decrypt path key off the SAME MSEK: the *public* half
    (`provision_recipient_mls_pubkey`) is the seal target; the *secret* half rides
    inside the sealed MLS snapshot the MDA opens bodies with. The seal-helper
    derives both from `recipient_msek` (per `mail-credentials.md` +
    `key-material-hierarchy.md`), so they match — exactly the production wiring a
    user's primary client performs; the nest never sees the MSEK.

    All via the production Admin/User-class WS-RPC writers (no SQLite poke):
    `create_account_alias` (User, as the recipient) + `provision_recipient_mls_pubkey` (Admin), then
    `provision_wrapped_mls_blob` + `provision_mls_snapshot_blob` (User, as the
    recipient). Shared by `mail_bridge_mda` and `mail_bridge_inbound_to_imap` so
    the MSEK-recipient wiring lives in one place.
    """
    import base64 as _b64
    import secrets
    from common.auth import create_actor_and_register as _create_actor
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    # Deferred: the helper's client imports nacl at module level, and the
    # shipped fakes suite loads this conftest without nacl installed.
    from helpers.mail_aliases import add_exact_alias
    from helpers.recipient_seal_key import provision_recipient_seal_key

    nest_port = nest_instance["port"]
    nest_url = nest_instance["url"]

    # `actor` provisions credentials for an EXISTING actor (e.g. the claimed
    # admin, whose handle is its only login name on a domainless home nest);
    # `domain=None` is that domainless case — no alias, and the username is the
    # bare handle a home-nest login resolves through the handle→actor store.
    recipient = actor if actor is not None else _create_actor(
        nest_port,
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    recipient_actor_id = recipient["actor_id_bytes"]
    username = local_part if domain is None else f"{local_part}@{domain}"

    recipient_msek = secrets.token_bytes(32)
    msek_b64 = _b64.b64encode(recipient_msek).decode()
    actor_id_b64 = _b64.b64encode(recipient_actor_id).decode()

    # Seal the same MSEK under `password` (credential_kind "plain" → Argon2id, the
    # family PLAIN AUTH resolves to) — the MUA-AUTH credential.
    wrapped_msek_blob = _run_seal_helper(seal_helper_binary, "seal-wrapped-msek", {
        "msek_b64": msek_b64,
        "actor_id_b64": actor_id_b64,
        "credential_id": "default",
        "credential_kind": "plain",
        "credential_b64": _b64.b64encode(password.encode()).decode(),
    })
    # Seal a v1 MLS snapshot carrying the MSEK-derived leaf init keypair, AEAD-
    # bound to the recipient actor_id (the MDA fetches + `cap.Decrypt`s it at AUTH,
    # then opens bodies with the leaf secret it carries).
    mls_snapshot_blob = _run_seal_helper(seal_helper_binary, "seal-mls-snapshot", {
        "msek_b64": msek_b64,
        "actor_id_b64": actor_id_b64,
    })

    admin = nest_instance["admin"]
    admin_ws = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        if domain is not None:
            add_exact_alias(nest_url, recipient["signing_key"], domain, local_part)
        provision_recipient_seal_key(
            admin_ws,
            recipient_actor_id,
            msek=recipient_msek,
            run_seal_helper=lambda s, p: _run_seal_helper(seal_helper_binary, s, p),
        )
    # provision_wrapped_mls_blob + provision_mls_snapshot_blob are User-class
    # self-registration (the blob is keyed on the caller's own actor_id), so
    # authenticate AS the recipient actor — mirroring the production path where
    # the user's own client uploads them.
    recipient_ws = WsRpcAdminClient(
        nest_url,
        actor_id=recipient_actor_id,
        signing_key=bytes(recipient["signing_key"]),
    )
    with recipient_ws:
        recipient_ws.call(
            "fauna.bridges.provision_wrapped_mls_blob",
            {
                "actor_id": recipient_actor_id,
                "credential_id": "default",
                "blob": wrapped_msek_blob,
            },
        )
        recipient_ws.call(
            "fauna.bridges.provision_mls_snapshot_blob",
            {"blob": mls_snapshot_blob},
        )

    return _MsekRecipient(
        recipient=recipient,
        actor_id=recipient_actor_id,
        username=username,
        password=password,
        msek=recipient_msek,
    )


def _actor_recipient_pubkey(*, db_path: str, actor_id: bytes) -> bytes:
    """The actor's standing MSEK-derived recipient X25519 public key, as the nest
    holds it (`actor_mls_pubkeys.mls_pubkey` — the row `get_recipient_seal_key`
    reads, `bins/fauna-nest/src/db/bridge_routing.rs`).

    This is the key every per-user spam artefact is sealed to: the model, a
    training-history row's subject and its delta (`mail-spam.md` § Encrypted-mode
    interaction). It exists only once the actor has mail enabled — provisioned by
    `_provision_msek_recipient`, or by the app's own enable
    (`MailSettingsActions.ensure_mail_enabled`). An actor without it has no MSEK,
    so it can have no per-user spam model or training history at all (nothing
    could seal one, nothing could read one) — by design, so this raises with that
    diagnosis instead of seeding a row no real writer could produce.

    A deadline poll (convention 14 — a ceiling, never a timing assertion): an
    app's enable publishes the key as one step of a multi-step provisioning, so
    a caller arriving straight from `ensure_mail_enabled()` may read a beat early.
    A key already on file returns on the first read.
    """
    import sqlite3
    import time as _time

    deadline = _time.monotonic() + 15.0
    while True:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=10.0)
        try:
            row = conn.execute(
                "SELECT mls_pubkey FROM actor_mls_pubkeys WHERE actor_id = ?1", (actor_id,),
            ).fetchone()
        finally:
            conn.close()
        if row is not None or _time.monotonic() >= deadline:
            break
        _time.sleep(0.25)
    if row is None:
        raise AssertionError(
            f"actor {actor_id.hex()} has no recipient key on file (actor_mls_pubkeys): "
            "a user without mail enabled has no MSEK, so no per-user spam model or "
            "training history can be sealed for it. Enable mail for the actor first "
            "(`_provision_msek_recipient`, or the app's `ensure_mail_enabled()`)."
        )
    return bytes(row[0])


def _seal_to_actor(*, seal_helper_binary: str, recipient_pubkey: bytes, plaintext: bytes) -> bytes:
    """Seal `plaintext` to an actor's own recipient X25519 key — the canonical
    `SealToRecipient` envelope (`seal-mail-record`). Byte-for-byte the shape a
    capability holder writes for a per-user spam artefact: the model re-seal
    (`apply_and_reseal`, the Go MDA's `trainJunkAgentSide`) and a training-history
    row's `sealed_subject` / `model_delta_applied`
    (`fauna_mail::spam::model_write::seal_history_blob`). The nest stores it
    verbatim and can never open it."""
    import base64 as _b64

    return _run_seal_helper(seal_helper_binary, "seal-mail-record", {
        "plaintext_b64": _b64.b64encode(plaintext).decode(),
        "recipient_x25519_pubkey_b64": _b64.b64encode(recipient_pubkey).decode(),
    })


def _seed_spam_model(
    *, db_path: str, actor_id: bytes, seal_helper_binary: str,
    ngrams: dict | None = None, spam_messages: int = 0, ham_messages: int = 0,
) -> bytes:
    """Seed a per-user `SpamModel` into the nest's `spam_models` table, **sealed
    at rest** to the actor's own recipient key — the only shape a model ever rests
    in (`mail-spam.md` § Encrypted-mode interaction: the nest stores what a
    capability holder sealed and can neither read nor write a model itself).
    Returns the sealed blob it stored.

    The plaintext inside the seal is the shared `fauna_mail::spam::SpamModel`
    serde_json shape (`libs/fauna-mail/src/spam/classifier.rs`): a version-tagged
    object with a sorted `ngrams` map of `token -> {spam, ham}` per-message
    occurrence counts plus the per-class message counters. `fetch_spam_model`
    returns the stored blob verbatim with `stored_sealed=true`; the MDA's
    SELECT-time scorer (`internal/mda/imap/spam_score.go`) and an app's on-device
    scorer open it under the actor's key and score against it — so seeding here
    *trains* the model for a tier_3 fixture **without** driving 50+ training
    gestures (the confidence ramp clamps the per-user term to 0 below
    `min_samples=50` and only reaches full weight at 200 — `mail-spam.md`
    § Combined-score formula). The seal→open→score path stays fully real; only the
    model's *contents* are a fixture. An agent-side `\\Junk` train opens this same
    blob, mutates and re-seals it (`store.go` `trainJunkAgentSide`), and any write
    re-seals with a fresh AEAD nonce, so the at-rest bytes provably change.

    `ngrams=None` seeds the minimal (empty) model. A hand-built JSON model couples
    to the serde shape only loosely: an incompatible shape makes
    `SpamModel::from_bytes` return `None` ⇒ a fresh empty model ⇒ weight 0 ⇒ no
    move ⇒ the e2e fails **loudly** (never a silent false-green). The SQLite poke
    mirrors the `put_spam_model` write (`bins/fauna-nest/src/db/moderation.rs`); a
    sealed write stores 0/0 in the `ham_count`/`spam_count` mirror columns (the
    model is nest-opaque), and so does this seed. The actor must have mail enabled
    (`_actor_recipient_pubkey`).
    """
    import json as _json
    import sqlite3
    import time as _time

    model = {
        "version": 1,
        "ngrams": {tok: {"spam": s, "ham": h} for tok, (s, h) in (ngrams or {}).items()},
        "spam_messages": spam_messages,
        "ham_messages": ham_messages,
    }
    sealed = _seal_to_actor(
        seal_helper_binary=seal_helper_binary,
        recipient_pubkey=_actor_recipient_pubkey(db_path=db_path, actor_id=actor_id),
        plaintext=_json.dumps(model).encode(),
    )
    now_millis = int(_time.time() * 1000)
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute(
            "INSERT INTO spam_models (actor_id, model_json, ham_count, spam_count, updated_at) "
            "VALUES (?1, ?2, 0, 0, ?3) "
            "ON CONFLICT(actor_id) DO UPDATE SET "
            "  model_json = excluded.model_json, "
            "  ham_count = excluded.ham_count, "
            "  spam_count = excluded.spam_count, "
            "  updated_at = excluded.updated_at",
            (actor_id, sealed, now_millis),
        )
        conn.commit()
    finally:
        conn.close()
    return sealed


def _seed_spam_baseline(*, db_path: str, ngrams: dict, spam_messages: int, ham_messages: int):
    """Seed the single-row deployment-baseline (`spam_baseline`, id=0) directly.

    Mirrors `Db::upsert_spam_baseline` (`bins/fauna-nest/src/db/moderation.rs`)
    — the same `fauna_mail::spam::SpamModel` serde_json shape as a per-user
    model. This stands in for the admin `publish_spam_baseline` aggregator (whose
    produce path is unit-tested in nest); the tier_3 value here is the **consume**
    path: `fetch_spam_model` folds this baseline into a cold-start actor's
    returned model as a faded read-time prior (`mail-spam.md` § Cold start,
    Path 2 step 4), which only the real
    seal→`OpenMailRecord`-unseal→shared scorer→`Move` path exercises. Seed/clear
    is bracketed inside the test (not the fixture), so the baseline exists only
    while the cold-start test runs — the other scoring tests are unperturbed
    regardless of order.
    """
    import json as _json
    import sqlite3
    import time as _time

    model = {
        "version": 1,
        "ngrams": {tok: {"spam": s, "ham": h} for tok, (s, h) in ngrams.items()},
        "spam_messages": spam_messages,
        "ham_messages": ham_messages,
    }
    model_json = _json.dumps(model).encode()
    now_millis = int(_time.time() * 1000)
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute(
            "INSERT INTO spam_baseline (id, model_json, ham_count, spam_count, contributors, published_at) "
            "VALUES (0, ?1, ?2, ?3, ?4, ?5) "
            "ON CONFLICT(id) DO UPDATE SET "
            "  model_json = excluded.model_json, "
            "  ham_count = excluded.ham_count, "
            "  spam_count = excluded.spam_count, "
            "  contributors = excluded.contributors, "
            "  published_at = excluded.published_at",
            (model_json, ham_messages, spam_messages, 1, now_millis),
        )
        conn.commit()
    finally:
        conn.close()


def _clear_spam_baseline(*, db_path: str):
    """Remove the published baseline (id=0) so a cold-start actor reverts to
    rspamd-only — the teardown half of `_seed_spam_baseline`."""
    import sqlite3

    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute("DELETE FROM spam_baseline WHERE id = 0")
        conn.commit()
    finally:
        conn.close()


def _provision_baseline_contributor(
    *, nest_instance, seal_helper_binary, local_part: str, token: str,
    spam_messages: int = 1, holder_x25519_pubkey: bytes | None = None, domain: str | None = None,
):
    """Stand up one opted-in, sealed, grant-bearing deployment-baseline contributor
    over the real WS-RPC wire — what an app's contribute-baseline toggle does
    (`mail-spam.md` § Encrypted-mode interaction). Returns `(contributor,
    grant_id)`, `contributor` an `_MsekRecipient`.

    The baseline is aggregated ONLY off-box, by the granted holder, over copies
    of sealed models: the nest can read no user's model, so it merges none
    itself. So a contributor is, in this order, what the app's toggle produces:

      - a full MSEK recipient (`_provision_msek_recipient`; `domain=None` skips
        the mail alias — a contributor needs a key, not an address);
      - opted in (`set_baseline_contribution`), after which the caller's own
        `fetch_spam_model` volunteers the box's aggregation holder
        (`holder_seal_target`) — used unless `holder_x25519_pubkey` names one;
      - a model sealed to its own recipient key (`seal-spam-model`) plus a copy
        sealed to the holder (`seal-spam-model-copy`, X-Wing when the target
        carries an ML-KEM key), written atomically by `put_spam_model`'s
        `holder_copy`;
      - a KEYLESS `content.read{spam-model}` grant minted to the holder
        (`mint-spam-model-grant` → `fauna.capabilities.mint`, as the owner) — it
        conveys no read of the contributor's mail.

    The model holds one spam token (`token`) over `spam_messages` spam samples
    and no ham, so a merged baseline's `sample_count` is the sum over the
    contributors it merged. A publish needs a holder enrolled on the nest (the
    session `mail_bridge_mda`, or a mail venue's MDA). The seal + grant-mint are
    fixture setup (E2E rule 8 carve-out (b)): the test-only seal-helper stands in
    for the user's primary app.
    """
    import base64 as _b64
    import json as _json
    import secrets
    import time as _time

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    def b64(raw: bytes) -> str:
        return _b64.b64encode(raw).decode()

    contributor = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=domain,
        local_part=local_part,
        password=f"{local_part}-password-1",
    )
    model_json = _json.dumps({
        "version": 1,
        "ngrams": {token: {"spam": spam_messages, "ham": 0}},
        "spam_messages": spam_messages,
        "ham_messages": 0,
    }).encode()

    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=contributor.actor_id,
        signing_key=bytes(contributor.recipient["signing_key"]),
    ) as ws:
        set_reply = ws.call("fauna.bridges.set_baseline_contribution", {"contribute": True})
        assert set_reply.get("contribute") is True, f"opt-in not ack'd: {set_reply!r}"

        holder_mlkem_ek = b""
        if holder_x25519_pubkey is None:
            target = ws.call(
                "fauna.bridges.fetch_spam_model", {"actor_id": contributor.actor_id},
            ).get("holder_seal_target")
            assert target, (
                "an opted-in contributor's fetch_spam_model volunteered no aggregation "
                "holder — no MDA / content-processor holder is enrolled on this nest"
            )
            holder_x25519_pubkey = bytes(target["x25519_pubkey"])
            holder_mlkem_ek = bytes(target.get("mlkem_ek") or b"")

        sealed_model = _run_seal_helper(seal_helper_binary, "seal-spam-model", {
            "msek_b64": b64(contributor.msek),
            "model_json_b64": b64(model_json),
        })
        sealed_copy = _run_seal_helper(seal_helper_binary, "seal-spam-model-copy", {
            "model_json_b64": b64(model_json),
            "owner_actor_id_b64": b64(contributor.actor_id),
            "holder_x25519_pubkey_b64": b64(holder_x25519_pubkey),
            "holder_mlkem_ek_b64": b64(holder_mlkem_ek) if holder_mlkem_ek else "",
        })
        grant_id = secrets.token_bytes(16)
        now = int(_time.time())
        grant_blob = _run_seal_helper(seal_helper_binary, "mint-spam-model-grant", {
            "owner_actor_id_b64": b64(contributor.actor_id),
            "grant_id_b64": b64(grant_id),
            "holder_pubkey_b64": b64(holder_x25519_pubkey),
            "epoch_start": now,
            "epoch_end": now + 3600,
        })

        ws.call("fauna.bridges.put_spam_model", {
            "sealed_model": sealed_model,
            "holder_copy": {"holder_pubkey": holder_x25519_pubkey, "sealed_copy": sealed_copy},
        })
        mint_reply = ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})
        assert mint_reply.get("ok") is True, f"grant mint not ok: {mint_reply!r}"

    return contributor, grant_id


def _withdraw_baseline_contributors(nest_instance, contributors) -> None:
    """Opt `contributors` (`_MsekRecipient`s) back out of the deployment baseline
    — the cleanup every test that stands contributors up on a SHARED nest owes
    its siblings: the baseline is a whole-nest aggregate, so a contributor left
    opted in is merged into every later publish on that nest."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    for contributor in contributors:
        with WsRpcAdminClient(
            nest_instance["url"],
            actor_id=contributor.actor_id,
            signing_key=bytes(contributor.recipient["signing_key"]),
        ) as ws:
            ws.call("fauna.bridges.set_baseline_contribution", {"contribute": False})


def _seed_spam_training_history(*, db_path: str, actor_id: bytes, seal_helper_binary: str, rows: list):
    """Seed `spam_training_history` rows straight into the nest DB for `actor_id`,
    **sealed** — the only shape a row rests in.

    Every row is written by a capability holder, never the nest: the app's
    sealed model write or the AUTH'd MDA's agent-side `\\Junk` train, both via
    `put_spam_model`'s `history_op: Insert` (`mail-spam.md` § Training-sample
    retention). So the subject and the forward n-gram delta are sealed to the
    actor's own recipient key (`_seal_to_actor` — the
    `fauna_mail::spam::model_write::seal_history_blob` shape) and land in
    `sealed_subject` / `model_delta_applied`; the nest stores both verbatim and
    can read neither. The SQLite poke mirrors that write
    (`Db::put_spam_model_with_history`, `bins/fauna-nest/src/db/moderation.rs`).

    What that means for a reader of these rows: `list_spam_training_history`
    returns the opaque `sealed_subject` with `message` degraded to the mailbox
    alone, and only an app holding the actor's MSEK unwraps the subject to render
    `"<subject> · <mailbox>"`; an undo is likewise client-side (the app unwraps
    the delta and writes the inverse + the row delete atomically). So a test that
    reads a seeded subject back or undoes a seeded row needs the app's mail
    enabled for this actor — which it must be anyway, since the rows are sealed to
    its recipient key (`_actor_recipient_pubkey` raises otherwise).

    Each `row` is a dict `{message_id: bytes, mailbox: str, subject: str,
    label: str, source: str}`, plus an optional `delta` (an iterable of n-gram
    strings, default empty); `label`/`source` are the snake_case wire tags
    (`spam`/`ham`; `imap_junk_flag`/`imap_junk_move`/`manual_other`). The delta's
    plaintext is the `encode_history_delta` JSON string array; an empty one undoes
    cleanly with no seeded model to invert (the inverse is all saturating
    subtraction). `created_at` is spaced 1 s apart ascending in list order, so the
    page's newest-first ordering (`created_at DESC`) returns `rows` reversed —
    deterministic. Returns the minted 16-byte `history_id`s in `rows` order.
    """
    import json as _json
    import sqlite3
    import time as _time
    import uuid

    recipient_pubkey = _actor_recipient_pubkey(db_path=db_path, actor_id=actor_id)

    def _seal(plaintext: bytes) -> bytes:
        return _seal_to_actor(
            seal_helper_binary=seal_helper_binary,
            recipient_pubkey=recipient_pubkey,
            plaintext=plaintext,
        )

    sealed_rows = [
        (
            _seal(row["subject"].encode()),
            _seal(_json.dumps(sorted(row.get("delta", ())), separators=(",", ":")).encode()),
        )
        for row in rows
    ]
    base_ms = int(_time.time() * 1000) - len(rows) * 1000
    ids = []
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        # Replace this actor's history with exactly `rows` so seeding tests are
        # order-independent against the session-shared nest.
        conn.execute("DELETE FROM spam_training_history WHERE actor_id = ?1", (actor_id,))
        for i, (row, (sealed_subject, sealed_delta)) in enumerate(zip(rows, sealed_rows)):
            hid = uuid.uuid4().bytes
            ids.append(hid)
            conn.execute(
                "INSERT INTO spam_training_history "
                "(history_id, actor_id, message_id, mailbox, label, source, "
                " model_delta_applied, created_at, sealed_subject) "
                "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                (
                    hid,
                    actor_id,
                    row["message_id"],
                    row["mailbox"],
                    row["label"],
                    row["source"],
                    sealed_delta,
                    base_ms + i * 1000,
                    sealed_subject,
                ),
            )
        conn.commit()
    finally:
        conn.close()
    return ids


class _MdaSpamScoring:
    """Three dedicated MDA recipients + the calibration the SELECT-time scoring
    tests share. `alice` carries a seeded per-user model that maps `spam_token`
    strongly to spam and `ham_token` strongly to ham (balanced priors, full
    confidence); `bob` is untrained; `carol` is untrained too, reserved for the
    cold-start deployment-baseline test (which seeds + clears the global
    `spam_baseline` row itself, keyed off `db_path`, using `baseline_spam_token`
    — distinct from alice's `spam_token` so the isolation test is unaffected).
    `mda` is the session bridge handle they all authenticate against.
    """

    def __init__(self, *, mda, alice, bob, carol, spam_token, ham_token, baseline_spam_token, db_path):
        self.mda = mda
        self.alice = alice
        self.bob = bob
        self.carol = carol
        self.spam_token = spam_token
        self.ham_token = ham_token
        self.baseline_spam_token = baseline_spam_token
        self.db_path = db_path


@pytest.fixture(scope="session")
def mda_spam_scoring(mail_bridge_mda, seal_helper_binary, nest_instance):
    """Provision the per-user spam-scoring tier_3 fixture against the running
    MDA bridge (`mail-spam.md` § Scoring placement — the post-delivery
    AUTH'd-MDA position; an internal design doc, § Slice 4 item 2).

    Two **dedicated** recipients (NOT the shared `mail_bridge_mda.recipient_*`),
    so the SELECT-time INBOX→Junk moves these tests drive never perturb the
    session-scoped recipient's INBOX that the IDLE / QRESYNC / CONDSTORE / SEARCH
    tests in this file accumulate into:

      - `alice` — a trained recipient. Its `spam_models` row is seeded so a
        message bearing `spam_token` scores ~10.4k milli (>> the default
        `spam_folder` = 5 → 5000 milli) and one bearing `ham_token` scores ~90
        milli (<< 5000). The priors are balanced (`spam_messages ==
        ham_messages`, 110 each ⇒ `sample_count` 220 ⇒ confidence clamps to 1.0)
        and the two distinctive tokens are nonce lowercase ASCII words that the
        shared tokenizer maps to a unigram of themselves (NFKC + Unicode-word +
        lowercase, ≥2 chars — `libs/fauna-mail/src/tokenizer.rs`), so the score
        is dominated by the prior + the one distinctive unigram (every other
        header/body token is unseen ⇒ skipped).
      - `bob` — an untrained recipient (no seeded model). `fetch_spam_model`
        returns `None` for him ⇒ cold start ⇒ the pass exits before scanning his
        INBOX, so the SAME `spam_token` message stays put — the per-actor
        isolation proof.

    No `put_spam_policy` is needed: the bridge backend seeds
    `SpamFolderThreshold = 5` from `wsrpc.DefaultSpamPolicyThresholds()` at boot
    (`internal/mda/imap/backend.go`), and the session snapshots it at connect.
    """
    domain = mail_bridge_mda.domain
    db_path = nest_instance["db_path"]

    # Nonce lowercase-ASCII tokens: collide with no other test message and are
    # invariant under the tokenizer's NFKC + lowercase + word-segmentation.
    # `baseline_spam_token` is DISTINCT from alice's `spam_token`, so the
    # cold-start-baseline test's published baseline never moves bob's
    # alice-`spam_token` mail (the isolation test stays valid regardless of order).
    spam_token = "qzspamtokenwx"
    ham_token = "qzhamtokenwx"
    baseline_spam_token = "qzbaselinespamwx"

    alice = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=domain,
        local_part="spam-alice",
        password="spam-alice-password-1",
    )
    bob = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=domain,
        local_part="spam-bob",
        password="spam-bob-password-1",
    )
    carol = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=domain,
        local_part="spam-carol",
        password="spam-carol-password-1",
    )

    # Train alice: each distinctive token appears in every message of its class
    # and none of the other, over balanced class counts (full confidence).
    _seed_spam_model(
        db_path=db_path,
        actor_id=alice.actor_id,
        seal_helper_binary=seal_helper_binary,
        ngrams={spam_token: (110, 0), ham_token: (0, 110)},
        spam_messages=110,
        ham_messages=110,
    )
    # carol is left untrained — the cold-start test seeds the deployment baseline
    # (not carol's own model) to prove she inherits it at read time.

    return _MdaSpamScoring(
        mda=mail_bridge_mda, alice=alice, bob=bob, carol=carol,
        spam_token=spam_token, ham_token=ham_token,
        baseline_spam_token=baseline_spam_token, db_path=db_path,
    )


class _MdaJunkTrainSealed:
    """Two dedicated MDA recipients for the agent-side `\\Junk`-train round-trip
    (`mail-spam.md` § Encrypted-mode interaction — every train is agent-side;
    there is no server relay):

      - `sealed` — a recipient whose `spam_models` row is seeded **sealed at
        rest** (`_seed_spam_model`), so `fetch_spam_model` reports
        `stored_sealed=true` and an IMAP `+\\Junk` STORE opens it, mutates it and
        re-seals it (`store.go` `trainJunkAgentSide`).
      - `untrained` — a recipient with **no** `spam_models` row, so
        `fetch_spam_model` reports `stored_sealed=false`: the MDA trains from an
        EMPTY model, seals it to the recipient's own key and writes it with a
        sealed history row — the cold-start arm of the same agent-side path.

    Fresh recipients (distinct local-parts), so training them never perturbs the
    session-shared `mail_bridge_mda.recipient_*` or the `mda_spam_scoring`
    recipients other tests depend on. `seeded_model` is the sealed blob at rest
    before the STORE (the baseline for the "re-sealed but still opaque" assertion).
    """

    def __init__(self, *, mda, sealed, untrained, seeded_model, db_path):
        self.mda = mda
        self.sealed = sealed
        self.untrained = untrained
        self.seeded_model = seeded_model
        self.db_path = db_path


@pytest.fixture(scope="session")
def mda_junk_train_sealed(mail_bridge_mda, seal_helper_binary, nest_instance):
    """Provision the agent-side `\\Junk`-train tier_3 fixture against the running
    MDA bridge (`mail-spam.md` § Encrypted-mode interaction). See
    `_MdaJunkTrainSealed`.

    Both recipients are full MSEK recipients (`_provision_msek_recipient`): the
    MSEK-derived recipient pubkey is the seal target and the sealed MLS snapshot
    carries the matching leaf secret, so the AUTH'd MDA session both opens the
    seeded sealed model and re-seals the mutated model to the same key — the whole
    seal→open→mutate→re-seal→write-back chain is real; only the seed model's
    *contents* are a fixture.
    """
    domain = mail_bridge_mda.domain
    db_path = nest_instance["db_path"]

    sealed = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=domain,
        local_part="junk-sealed",
        password="junk-sealed-password-1",
    )
    untrained = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=domain,
        local_part="junk-untrained",
        password="junk-untrained-password-1",
    )
    # Seed `sealed`'s model at rest so the MDA opens and mutates a stored model;
    # `untrained` is left without one, so the MDA trains from an empty model.
    seeded_model = _seed_spam_model(
        db_path=db_path,
        actor_id=sealed.actor_id,
        seal_helper_binary=seal_helper_binary,
    )

    return _MdaJunkTrainSealed(
        mda=mail_bridge_mda, sealed=sealed, untrained=untrained,
        seeded_model=seeded_model, db_path=db_path,
    )


@pytest.fixture(scope="session")
def mail_bridge_mda(mail_bridge_binary, seal_helper_binary, nest_instance, tmp_path_factory):
    """Spawn fauna-mail-bridge in MDA role against `nest_instance`.

    This fixture owns the **nest-global** deployment state (provisioned once,
    reused read-only by any MDA bridge spawned against this nest): the
    claimed local domain and a pre-provisioned recipient actor. It then
    delegates the **per-bridge** spawn
    (keypair, register_user, admin enrollment, x25519 hand-poke, self-signed
    cert, ephemeral ports, operator-hatch, process) to `_spawn_mda_bridge` and
    yields a `MailBridgeMDAHandle`.

    Two material differences from `mail_bridge_mta`:

    (1) TLS is mandatory. `mda.Run` hard-gates on
        `TLSProvider != nil` (`bins/fauna-bridges/internal/mda/
        mda.go:129-131`); listeners 993 (IMAPS), 143 (IMAP+STARTTLS),
        443 (CalDAV) all serve over a `tls.Config` whose
        `GetCertificate` reads the sealed `TlsCertBlob` the bridge
        fetched via `fauna.bridges.fetch_tls_cert_blob`. `_spawn_mda_bridge`
        provisions a self-signed cert through the admin WS-RPC kind
        `fauna.bridges.provision_self_signed_cert`
        (landed internally), which calls
        `Storage::store_acme_material(...)` and so seals the cert to
        the bridge's x25519 pubkey + stores it in `bridge_tls_cert_blobs`.

    (2) Bridge x25519 chicken-and-egg. Production path: bridge starts,
        opens WS-RPC, calls `fauna.bridges.register_service_user` with
        its x25519 pubkey, the row in `bridge_service_users` is updated
        from NULL → pubkey. **For the MDA**, the bridge can't open WS-RPC
        until it has a TLS cert; the TLS cert can't be sealed until the
        x25519 is in DB. `_spawn_mda_bridge` sidesteps with a SQLite
        hand-poke of `bridge_service_users.x25519_pubkey` BEFORE the cert
        provisioning step — same workaround pattern as `mail_bridge_mta`'s
        `recipient_routes` hand-poke.
        The mail-bridge sealing review
        covers the right production shape (admin approve auto-inserts
        users row + seal-helper for admin-issued blobs). The enrollment
        kind already carries an optional `x25519_pubkey` (bound set-once at
        `request_enrollment`); routing the harness's pre-approve through it
        is the shape that would retire the hand-poke.

    The split into nest-global state here + per-bridge `_spawn_mda_bridge` is
    what lets `disposable_mda_bridge` stand up a *second* bridge on this same
    nest-global state for the destructive graceful-shutdown test without
    stranding this session-scoped one.

    Process safety: owns its spawned subprocess (via `_SpawnedMdaBridge`),
    tracks it through drivers/port_util atexit cleanup, never pkill/killall.
    """
    tmp = tmp_path_factory.mktemp("mail-bridge-mda")
    domain = MAIL_PRIMARY_DOMAIN
    bridge_id = "test-mda-1"
    recipient_local_part = "mda-recipient"

    nest_port = nest_instance["port"]
    nest_url = nest_instance["url"]

    # ── 1. Admin claims the local domain over WS-RPC (no-HTTP directive). Must
    # precede `_provision_msek_recipient` (§ 2), whose alias write
    # resolves through `local_domain=domain`. `is_primary` is nest-derived
    # (first active domain = primary), and the autouse
    # `_session_primary_mail_domain` fixture already claimed
    # `MAIL_PRIMARY_DOMAIN` on the session nest, so this call is either a
    # `skipped: true` re-add (same domain as the autouse fixture) or a fresh
    # non-primary add — never accidentally a fresh primary.
    from clients.ws_rpc_admin_client import WsRpcAdminClient as _WsRpcAdminClient
    _admin = nest_instance["admin"]
    _domain_ws = _WsRpcAdminClient(
        nest_url,
        actor_id=bytes(_admin["signing_key"].verify_key),
        signing_key=bytes(_admin["signing_key"]),
    )
    with _domain_ws:
        # Stage-5 default-off seed: enable mail explicitly BEFORE the MDA's
        # first `fetch_config` (an unset toggle reads OFF, and the MDA binds
        # its listener set once at startup). Mirrors production's claim-time
        # § 3b glue / admin-mail toggle; idempotent with `mail_bridge_mta`'s
        # identical seed on the shared nest.
        _domain_ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        is_primary = _domain_ws.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": domain,
                "mta_sts_cert_mode": "per_host",
            },
        )["domain"]["is_primary"]
    _ = is_primary  # surfaced through tracing only — no handle field needed today

    # ── 2. Pre-provision the recipient actor + its MSEK-derived credentials
    # (exact alias, MLS pubkey, wrapped-MSEK blob, MLS snapshot — all keyed off
    # one MSEK so the seal/open halves match). Shared with the inbound→IMAP-read
    # fixture via `_provision_msek_recipient`.
    prov = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=domain,
        local_part=recipient_local_part,
        password="mda-test-password-1",
    )
    recipient = prov.recipient
    recipient_username = prov.username
    recipient_password = prov.password

    # ── 2b. Co-resident fake clamd + rspamd (benign verdicts) wired onto the MDA
    # via the operator-hatch — the re-score drain (`internal/mda/rescore_drain.go`)
    # re-runs these same scanners when it drains a stale content_scores row under a
    # capability grant (the `test_capability_rescore_drain` tier_3 test). Inert for
    # every other MDA test: the drain only fires when a holder has live grants, and
    # nothing else here mints one. Same in-process-daemon posture as the MTA
    # fixture's scanner block.
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd
    fake_clamd = FakeClamd().start()
    fake_rspamd = FakeRspamd().start()
    mda_scan_hatch = (
        f'clamd_addr = "{fake_clamd.addr}"\n'
        f'rspamd_url = "{fake_rspamd.url}"\n'
    )

    # ── 3. Delegate the per-bridge spawn (keypair, enrollment, x25519
    # hand-poke, self-signed cert, ephemeral ports, process).
    spawned = _spawn_mda_bridge(
        mail_bridge_binary=mail_bridge_binary,
        nest_instance=nest_instance,
        tmp=tmp,
        bridge_id=bridge_id,
        domain=domain,
        operator_hatch_extra=mda_scan_hatch,
    )

    try:
        handle = MailBridgeMDAHandle(
            proc=spawned.proc,
            imaps_port=spawned.imaps_port,
            imap_starttls_port=spawned.imap_starttls_port,
            caldav_port=spawned.caldav_port,
            metrics_port=spawned.metrics_port,
            domain=domain,
            bridge_id=spawned.bridge_id,
            bridge_role="mda",
            recipient_actor=recipient,
            recipient_username=recipient_username,
            recipient_password=recipient_password,
            keypair_file=spawned.keyfile_path,
            log_file=spawned.log_file_path,
            tls_trust_anchor=None,
            ed25519_pubkey=spawned.ed25519_pubkey,
            x25519_pubkey=spawned.x25519_pubkey,
            x25519_secret=spawned.x25519_secret,
        )
        yield handle
    finally:
        spawned.cleanup()
        fake_clamd.stop()
        fake_rspamd.stop()


@pytest.fixture
def dav_user(mail_bridge_mda, nest_instance, seal_helper_binary):
    """Factory: a fresh DAV + mail user on the session MDA's domain.

    ``dav_user("label")`` returns ``(username, password, actor)`` — a new actor
    with its own exact alias, MSEK-derived keys and credential, minted through
    the same production writers ``mail_bridge_mda`` uses for its recipient. The
    nest-outcome DAV witnesses take one per role so no test shares a calendar,
    an address book, an inbox or a lockout bucket with another.
    """
    import secrets as _secrets

    def make(label: str):
        local = f"{label}-{_secrets.token_hex(4)}"
        prov = _provision_msek_recipient(
            nest_instance=nest_instance,
            seal_helper_binary=seal_helper_binary,
            domain=mail_bridge_mda.domain,
            local_part=local,
            password=f"{local}-password",
        )
        return prov.username, prov.password, prov.recipient

    return make


@pytest.fixture
def disposable_mda_bridge(
    mail_bridge_mda, mail_bridge_binary, nest_instance, tmp_path_factory,
):
    """A throwaway MDA bridge for *destructive* tests (the graceful-shutdown
    SIGTERM drain), so a test can kill a bridge without stranding the
    session-scoped one every other test in the file shares.

    Depending on `mail_bridge_mda` guarantees the nest-global deployment state
    (claimed local domain, recipient actor) is already provisioned; this
    fixture only adds a *second* bridge process with its own
    keypair + enrollment + x25519 hand-poke + ephemeral ports, reusing that
    state read-only. The unique `bridge_id` gives it a distinct nest WS actor
    (two bridges on one keypair would collide). Its
    `fauna.bridges.provision_self_signed_cert` WS-RPC call re-seals the domain
    cert to its own x25519 without disturbing the
    already-running session bridge (which fetched its cert once at startup).
    """
    session = mail_bridge_mda
    tmp = tmp_path_factory.mktemp("mail-bridge-mda-disposable")
    spawned = _spawn_mda_bridge(
        mail_bridge_binary=mail_bridge_binary,
        nest_instance=nest_instance,
        tmp=tmp,
        bridge_id=f"test-mda-disposable-{uuid.uuid4().hex[:12]}",
        domain=session.domain,
    )
    try:
        handle = MailBridgeMDAHandle(
            proc=spawned.proc,
            imaps_port=spawned.imaps_port,
            imap_starttls_port=spawned.imap_starttls_port,
            caldav_port=spawned.caldav_port,
            metrics_port=spawned.metrics_port,
            domain=session.domain,
            bridge_id=spawned.bridge_id,
            bridge_role="mda",
            recipient_actor=session.recipient_actor,
            recipient_username=session.recipient_username,
            recipient_password=session.recipient_password,
            keypair_file=spawned.keyfile_path,
            log_file=spawned.log_file_path,
            tls_trust_anchor=None,
            ed25519_pubkey=spawned.ed25519_pubkey,
            x25519_pubkey=spawned.x25519_pubkey,
            x25519_secret=spawned.x25519_secret,
        )
        yield handle
    finally:
        spawned.cleanup()


def _bench_mda_impl(
    *, mail_bridge_binary, seal_helper_binary, nest_binary, tmp_path_factory,
    mode: str, label: str,
):
    """Stand up a DEDICATED nest + MDA labeled ``mode``, with a freshly
    provisioned MSEK recipient — the substrate for the Phase-3 IMAP
    FETCH-latency benchmark (``test_imap_fetch_latency_bench.py``).

    NOTE (no-modes retirement, ratified 2026-07-12): the nest no longer has a
    storage-mode axis — every nest is sealed at rest unconditionally (the
    constant ``fetch_config.storage_mode`` shim that outlived it left the wire
    2026-09-24; `docs/goal/architecture/nest/storage-modes.md`).
    So the ``"plaintext"`` vs ``"encrypted"`` label this fixture still accepts
    no longer selects a different server-side serve path — both arms now
    exercise the same ``OpenMailRecord`` HPKE-open. The benchmark's plaintext-
    vs-encrypted delta measurement (still described in
    ``test_imap_fetch_latency_bench.py``'s docstring) is therefore stale as of
    this retirement; a follow-on should either collapse the two-arm shape or
    retire the benchmark. Kept functional (not deleted) here since redesigning
    an opt-in perf benchmark is out of scope for the fixture sweep that
    produced this note.

    A near-mirror of :func:`mail_bridge_mda`, with two deliberate differences:

      (1) It runs on its **own** ``_make_nest`` rather than the session
          ``nest_instance`` — kept isolated per label so the benchmark's
          per-arm bulk APPEND never shares a mailbox across arms.
      (2) It claims ``MAIL_PRIMARY_DOMAIN`` itself. The autouse
          ``_session_primary_mail_domain`` only pins the SESSION nest, so on a
          dedicated nest this is the first (and therefore primary) domain claim.

    Everything else — the MSEK recipient provision (whose pubkey/snapshot are
    keyed off one MSEK so the seal-at-APPEND / open-at-FETCH halves match), the
    inert co-resident fake scanners, the ``_spawn_mda_bridge`` delegation, the
    ``MailBridgeMDAHandle`` shape, and the reverse-order teardown — is
    identical to ``mail_bridge_mda``. The handle carries an extra
    ``storage_mode`` attribute so the benchmark can label its rows.

    Function-scoped isolation + own nest ⇒ the benchmark's bulk APPEND never
    pollutes the session ``mail_bridge_mda`` recipient's INBOX for sibling tests.

    Process safety: owns its nest + bridge + scanner subprocesses; tears them
    down in reverse order on teardown, never pkill/killall.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient as _WsRpcAdminClient
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd

    # No domain start option: the `add_local_domain` below registers
    # MAIL_PRIMARY_DOMAIN, and registering the PRIMARY is what sets the
    # deployment identity (`apply_primary_identity`) — so the domain is live
    # well before this fixture yields. A `claim_domain` here would be a second
    # door onto the same domain, and `add_local_domain` being idempotent by
    # NAME would answer the loser with `skipped: true`, silently discarding
    # its `per_host` cert mode (`testing.md` § Default app and nest mode,
    # ruling (3)).
    nest, nest_cleanup = _make_nest(nest_binary, tmp_path_factory, label)
    tmp = tmp_path_factory.mktemp(label + "-bridge")
    domain = MAIL_PRIMARY_DOMAIN
    bridge_id = f"bench-mda-{mode}"
    recipient_local_part = f"bench-{mode}-recipient"
    admin = nest["admin"]

    try:
        # ── 1. Admin claims the primary local domain (dedicated nest has none).
        admin_ws = _WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            # Stage-5 default-off seed: enable mail explicitly before the MDA's
            # first `fetch_config` (an unset toggle reads OFF) — same seed as
            # `mail_bridge_mda`.
            admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})
            admin_ws.call(
                "fauna.bridges.add_local_domain",
                {"domain": domain,
                 "mta_sts_cert_mode": "per_host"},
            )

        # ── 2. Pre-provision the recipient actor + its MSEK-derived credentials
        # (exact alias, MLS pubkey, wrapped-MSEK blob, MLS snapshot — all keyed
        # off one MSEK so the seal/open halves match).
        prov = _provision_msek_recipient(
            nest_instance=nest,
            seal_helper_binary=seal_helper_binary,
            domain=domain,
            local_part=recipient_local_part,
            password=f"bench-{mode}-password-1",
        )

        # ── 2b. Co-resident fake clamd + rspamd — inert (no capability holder
        # mints a grant here, so the re-score drain never fires). Present only to
        # mirror `mail_bridge_mda`'s scanner wiring exactly.
        fake_clamd = FakeClamd().start()
        fake_rspamd = FakeRspamd().start()
        mda_scan_hatch = (
            f'clamd_addr = "{fake_clamd.addr}"\n'
            f'rspamd_url = "{fake_rspamd.url}"\n'
        )

        # ── 3. Delegate the per-bridge spawn against THIS nest.
        spawned = _spawn_mda_bridge(
            mail_bridge_binary=mail_bridge_binary,
            nest_instance=nest,
            tmp=tmp,
            bridge_id=bridge_id,
            domain=domain,
            operator_hatch_extra=mda_scan_hatch,
        )
        try:
            handle = MailBridgeMDAHandle(
                proc=spawned.proc,
                imaps_port=spawned.imaps_port,
                imap_starttls_port=spawned.imap_starttls_port,
                caldav_port=spawned.caldav_port,
                metrics_port=spawned.metrics_port,
                domain=domain,
                bridge_id=spawned.bridge_id,
                bridge_role="mda",
                recipient_actor=prov.recipient,
                recipient_username=prov.username,
                recipient_password=prov.password,
                keypair_file=spawned.keyfile_path,
                log_file=spawned.log_file_path,
                tls_trust_anchor=None,
                ed25519_pubkey=spawned.ed25519_pubkey,
                x25519_pubkey=spawned.x25519_pubkey,
                x25519_secret=spawned.x25519_secret,
            )
            handle.storage_mode = mode  # benchmark row label
            yield handle
        finally:
            spawned.cleanup()
            fake_clamd.stop()
            fake_rspamd.stop()
    finally:
        nest_cleanup()


@pytest.fixture(scope="function")
def bench_mda_plaintext(
    mail_bridge_binary, seal_helper_binary, bench_nest_binary, tmp_path_factory,
):
    """Dedicated plaintext-mode RELEASE nest + MDA + fresh recipient for the
    Phase-3 IMAP FETCH-latency benchmark. See :func:`_bench_mda_impl`."""
    yield from _bench_mda_impl(
        mail_bridge_binary=mail_bridge_binary,
        seal_helper_binary=seal_helper_binary,
        nest_binary=bench_nest_binary,
        tmp_path_factory=tmp_path_factory,
        mode="plaintext",
        label="bench-mda-pt-nest",
    )


@pytest.fixture(scope="function")
def bench_mda_encrypted(
    mail_bridge_binary, seal_helper_binary, bench_nest_binary, tmp_path_factory,
):
    """Dedicated encrypted-mode RELEASE nest + MDA + fresh recipient for the
    Phase-3 IMAP FETCH-latency benchmark — the arm whose FETCH pays the
    per-message ``OpenMailRecord`` HPKE-open the benchmark measures. See
    :func:`_bench_mda_impl`."""
    yield from _bench_mda_impl(
        mail_bridge_binary=mail_bridge_binary,
        seal_helper_binary=seal_helper_binary,
        nest_binary=bench_nest_binary,
        tmp_path_factory=tmp_path_factory,
        mode="encrypted",
        label="bench-mda-enc-nest",
    )


@pytest.fixture(scope="function")
def restartable_mda_nest(
    mail_bridge_binary, seal_helper_binary, nest_binary, tmp_path_factory,
):
    """A DEDICATED, restart-capable nest + MDA + one MSEK recipient — for
    tier_3 tests that must restart a real nest to exercise boot-time passes
    (`test_dav_content_at_rest_e2e.py`, `test_dr_restore.py`).

    Renamed from its old seal-backfill name (2026-09-30) to say what it is
    now: this fixture first served the S4 seal-backfill tier_3, retired
    2026-08-17 with the record-identity cutover along with the whole
    `content_seal_backfill` module. What it exists for now is any tier_3
    needing a real nest RESTART: the DAV at-rest conformance walk, the
    placement-manifest boot heal, and DR restore.

    A near-mirror of `_bench_mda_impl` (own `_make_nest`, claimed primary
    domain, one MSEK recipient, one MDA bridge) — but function-scoped
    isolation is REQUIRED here, not just preferred: the test calls
    `common.nest.restart_nest` on `handle.nest_instance` to exercise a real
    boot-time pass (`bins/fauna-nest/src/lib.rs`), and a nest restart must
    never land on the shared session `nest_instance` every other mail test
    depends on.

    `handle.nest_instance` (the private `nest` dict `restart_nest`/`stop_nest`/
    `start_nest_in_place` operate on) and `handle.recipient_msek` (raw bytes,
    in case a test wants to open a record test-side) are attached past
    `MailBridgeMDAHandle.__init__` — same pattern as `_bench_mda_impl`'s
    `handle.storage_mode`.

    Process safety: owns its nest + bridge + scanner subprocesses; tears them
    down in reverse order on teardown, never pkill/killall.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient as _WsRpcAdminClient
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd

    # No domain start option: the `add_local_domain` below registers
    # MAIL_PRIMARY_DOMAIN, and registering the PRIMARY is what sets the
    # deployment identity (`apply_primary_identity`) — so the domain is live
    # well before this fixture yields. A `claim_domain` here would be a second
    # door onto the same domain, and `add_local_domain` being idempotent by
    # NAME would answer the loser with `skipped: true`, silently discarding
    # its `per_host` cert mode (`testing.md` § Default app and nest mode,
    # ruling (3)).
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "content-seal-backfill",
    )
    tmp = tmp_path_factory.mktemp("content-seal-backfill-bridge")
    domain = MAIL_PRIMARY_DOMAIN
    admin = nest["admin"]

    try:
        admin_ws = _WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            # Stage-5 default-off seed: enable mail explicitly before the MDA's
            # first `fetch_config` (an unset toggle reads OFF) — same seed as
            # `mail_bridge_mda`.
            admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})
            admin_ws.call(
                "fauna.bridges.add_local_domain",
                {"domain": domain,
                 "mta_sts_cert_mode": "per_host"},
            )

        prov = _provision_msek_recipient(
            nest_instance=nest,
            seal_helper_binary=seal_helper_binary,
            domain=domain,
            local_part="seal-backfill-recipient",
            password="seal-backfill-test-password-1",
        )

        fake_clamd = FakeClamd().start()
        fake_rspamd = FakeRspamd().start()
        mda_scan_hatch = (
            f'clamd_addr = "{fake_clamd.addr}"\n'
            f'rspamd_url = "{fake_rspamd.url}"\n'
        )

        spawned = _spawn_mda_bridge(
            mail_bridge_binary=mail_bridge_binary,
            nest_instance=nest,
            tmp=tmp,
            bridge_id="content-seal-backfill-mda",
            domain=domain,
            operator_hatch_extra=mda_scan_hatch,
        )
        try:
            handle = MailBridgeMDAHandle(
                proc=spawned.proc,
                imaps_port=spawned.imaps_port,
                imap_starttls_port=spawned.imap_starttls_port,
                caldav_port=spawned.caldav_port,
                metrics_port=spawned.metrics_port,
                domain=domain,
                bridge_id=spawned.bridge_id,
                bridge_role="mda",
                recipient_actor=prov.recipient,
                recipient_username=prov.username,
                recipient_password=prov.password,
                keypair_file=spawned.keyfile_path,
                log_file=spawned.log_file_path,
                tls_trust_anchor=None,
                ed25519_pubkey=spawned.ed25519_pubkey,
                x25519_pubkey=spawned.x25519_pubkey,
                x25519_secret=spawned.x25519_secret,
            )
            handle.nest_instance = nest
            handle.recipient_msek = prov.msek
            yield handle
        finally:
            spawned.cleanup()
            fake_clamd.stop()
            fake_rspamd.stop()
    finally:
        nest_cleanup()


@pytest.fixture(scope="function")
def dav_toggle_venue(nest_binary, mail_bridge_binary, seal_helper_binary, tmp_path_factory):
    """Factory: a dedicated nest + one DAV user + one MDA, with the deployment's
    mail / CalDAV / CardDAV toggles set as asked BEFORE the MDA boots.

    ``dav_toggle_venue(mail=…, caldav=…, carddav=…, webdav=…)`` returns a
    ``MailBridgeMDAHandle`` (plus ``handle.nest_instance``). A toggle passed as
    ``None`` is left unset — the "follows mail" state
    (`carddav-server.md` § Independent enablement). The toggles are set before
    the spawn because the MDA binds its listener set once, from its first
    ``fetch_config``; flipping one later would make it exit for a rebind no
    supervisor is here to answer.

    For the enablement witnesses that must not flip the shared session nest's
    toggles under every other mail test: contacts served with mail off, and
    contacts switched off. The domain is registered (and so becomes the
    deployment identity) whatever mail says, so the apex well-known has a host
    to name. ``domainless=True`` is the home-nest shape instead: no domain at
    all, the MDA on the nest's self-signed floor cert, and the claimed admin —
    the only handled actor — signing in by bare handle (``admin``). Standalone-only for the same reason as
    ``restartable_mda_nest`` — it spawns its own binaries.

    Process safety: owns its nests + bridges; tears them down in reverse order,
    never pkill/killall.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient as _WsRpcAdminClient

    teardown = []

    def make(*, mail, caldav=None, carddav=None, webdav=None, domainless=False):
        label = f"dav-toggle-{len(teardown)}"
        nest, nest_cleanup = _make_nest(nest_binary, tmp_path_factory, label)
        teardown.append(nest_cleanup)
        admin = nest["admin"]
        admin_ws = _WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        domain = None if domainless else MAIL_PRIMARY_DOMAIN
        with admin_ws:
            admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": mail})
            for kind, value in (("caldav", caldav), ("carddav", carddav), ("webdav", webdav)):
                if value is not None:
                    admin_ws.call(f"fauna.bridges.set_{kind}_enabled", {"enabled": value})
            if domain is not None:
                admin_ws.call(
                    "fauna.bridges.add_local_domain",
                    {"domain": domain,
                     "mta_sts_cert_mode": "per_host"},
                )
        if domainless:
            # A home nest's only login name is the claimed admin's handle (the
            # claim's default `admin`), resolved through the handle→actor store.
            owner = {
                "actor_id_bytes": bytes(admin["signing_key"].verify_key),
                "actor_id_hex": bytes(admin["signing_key"].verify_key).hex(),
                "signing_key": admin["signing_key"],
            }
            prov = _provision_msek_recipient(
                nest_instance=nest, seal_helper_binary=seal_helper_binary, domain=None,
                local_part="admin", password="dav-home-password-1", actor=owner,
            )
        else:
            prov = _provision_msek_recipient(
                nest_instance=nest,
                seal_helper_binary=seal_helper_binary,
                domain=domain,
                local_part="dav-toggle",
                password="dav-toggle-password-1",
            )
        spawned = _spawn_mda_bridge(
            mail_bridge_binary=mail_bridge_binary,
            nest_instance=nest,
            tmp=tmp_path_factory.mktemp(f"{label}-bridge"),
            bridge_id=f"{label}-mda",
            domain=domain,
        )
        teardown.append(spawned.cleanup)
        handle = MailBridgeMDAHandle(
            proc=spawned.proc,
            imaps_port=spawned.imaps_port,
            imap_starttls_port=spawned.imap_starttls_port,
            caldav_port=spawned.caldav_port,
            metrics_port=spawned.metrics_port,
            domain=domain,
            bridge_id=spawned.bridge_id,
            bridge_role="mda",
            recipient_actor=prov.recipient,
            recipient_username=prov.username,
            recipient_password=prov.password,
            keypair_file=spawned.keyfile_path,
            log_file=spawned.log_file_path,
            tls_trust_anchor=None,
            ed25519_pubkey=spawned.ed25519_pubkey,
            x25519_pubkey=spawned.x25519_pubkey,
            x25519_secret=spawned.x25519_secret,
        )
        handle.nest_instance = nest
        return handle

    try:
        yield make
    finally:
        for cleanup in reversed(teardown):
            cleanup()


class MailVenueBridges:
    """The two questions a test asks about a mail venue's BRIDGES, answered by
    the venue rather than by the test.

    Both mail-venue handles below mix this in, and both used to answer these
    questions by handing out the raw machinery instead: `mta_proc`, a
    `subprocess.Popen`, and `mta_log_file` / `mda_log_file`, paths on the HOST
    filesystem. Twenty-five test files then re-derived the same two answers from
    them, the liveness one as a byte-identical three-line assert copied
    twenty-nine times.

    **That is a standalone-only fact wired into every consuming test body.**
    `testing.md` § Default app and nest mode, ruling (3) settles the docker shape
    of a mail nest as the image's own s6-supervised bridges, and names the MTA
    log file and `mda.respawn()` among that mode's declared absences: there is no
    host process to poll and no host path to print when the bridges are services
    inside a container. The same two questions are perfectly answerable
    there — `s6-svstat` says whether the MTA is up and `docker logs` is where its
    output went — but only by something that knows how the bridges run.

    So the handle answers them. It is the same move `start_in_place` made for
    restarting a nest ("the provider that knows how to START this nest also
    answers how to start it AGAIN"), and it is what lets a container venue
    satisfy fifty-eight call sites by overriding two methods instead of
    fifty-eight tests learning a second shape. Pinned by
    `test_nest_mode_axis.py::test_no_test_reads_a_bridge_process_or_log_path_directly`,
    which is what stops the raw reads growing back.
    """

    def assert_mta_running(self) -> None:
        """Precondition: this venue's MTA bridge is up before the test runs.

        Raises with the venue's own log pointer attached, because a dead MTA is
        never diagnosable from the assertion alone. A venue that runs NO MTA (the
        CalDAV-only variant) fails here too, and deliberately: a test asserting
        this has stated that it needs one, and the old raw idiom answered that
        case with an `AttributeError` on `None`.
        """
        proc = self.mta_proc
        if proc is None:
            raise AssertionError(
                "this mail venue runs no MTA bridge (the CalDAV-only variant "
                "spawns none), so a test that requires one cannot run against it"
            )
        code = proc.poll()
        if code is not None:
            raise AssertionError(
                f"the MTA bridge exited (status {code}) before the test ran; "
                f"{self.bridge_log_hint('mta')}"
            )

    def bridge_log_hint(self, role: str = "mta") -> str:
        """Where this venue's `role` bridge output went, as a clause a failure
        message can embed.

        A STRING rather than a path, because the answer is not a path in every
        mode — a container venue's answer is a `docker logs` invocation — and a
        message that interpolates it should not have to care which. Never raises
        and never returns empty: a venue that cannot answer says so, which is
        strictly better inside an `except` block than the `AttributeError` that
        stood here before (`test_caldav_client_seal_to_mua.py` had already
        hand-rolled exactly this fallback for the MDA).
        """
        assert role in ("mta", "mda"), f"unknown bridge role {role!r}"
        path = getattr(self, f"{role}_log_file", None)
        if path is None:
            return f"({role} log not exposed by this mail venue)"
        return f"{role} log: {path}"

    def bridge_log_lines(self, role: str = "mta") -> list[str]:
        """This venue's `role` bridge output, as lines, newest last.

        The read-it half of `bridge_log_hint`, for a diagnostic that greps the
        log rather than pointing at it. Same reason to live here: standalone
        opens a file the fixture redirected, a container venue would shell out to
        `docker logs`, and a caller that only wants the lines should not have to
        know which. Empty on a venue that cannot answer — a diagnostic helper
        runs inside a failure path, where raising would replace the assertion
        that actually failed (the trap `log_path` sprang on
        `test_apple_track_a_diag.py`).
        """
        assert role in ("mta", "mda"), f"unknown bridge role {role!r}"
        path = getattr(self, f"{role}_log_file", None)
        if path is None:
            return []
        try:
            with open(path, "r", errors="replace") as fh:
                return [line.rstrip() for line in fh]
        except OSError:
            return []


class MailBridgeInboundToImapHandle(MailVenueBridges):
    """Handle for the `mail_bridge_inbound_to_imap` fixture — the full inbound→
    IMAP-read seam: one MTA bridge (SMTP) + one reused MDA bridge (IMAP) on the
    SAME local domain, serving the SAME fresh MSEK recipient against one nest.

    Exposes `mx_port` (the MTA's port-25 inbound listener) and `imaps_port` +
    `domain` (the reused MDA bridge's IMAPS listener — named so the shared
    `_imaps_connect` helper duck-types on this handle directly), plus the fresh
    recipient's MUA-AUTH credentials. `mta_proc` is the MTA subprocess for a
    liveness probe; `mta_log_file` points at its log for failure diagnostics.
    """

    def __init__(
        self, *, mx_port, imaps_port, domain, recipient_username,
        recipient_password, mta_proc, mta_log_file, mda, recipient=None,
    ):
        self.mx_port = mx_port
        self.imaps_port = imaps_port
        self.domain = domain
        self.recipient_username = recipient_username
        self.recipient_password = recipient_password
        self.mta_proc = mta_proc
        self.mta_log_file = mta_log_file
        self.mda = mda
        # The full `_MsekRecipient` (actor_id, msek, and `.recipient["signing_key"]`)
        # — the capability re-score-drain test mints a `content.read{mail}` grant
        # from this owner's MSEK to the MDA holder. Other consumers use only the
        # MUA-AUTH username/password above.
        self.recipient = recipient


@pytest.fixture(scope="session")
def mail_bridge_inbound_to_imap(
    mail_bridge_mda, mail_bridge_binary, seal_helper_binary, nest_instance,
    tmp_path_factory,
):
    """Stand up the full inbound→IMAP-read seam on real binaries (NO docker).

    Reuses `mail_bridge_mda` wholesale for the *read* half — its nest, its MDA
    IMAPS bridge, and its claimed local domain — and adds the *receive* half
    on the SAME domain:

      - a FRESH dedicated MSEK recipient (so the recipient's INBOX is clean and
        the inbound message is the deterministic `1 EXISTS`, with no accumulation
        from the sibling MDA tests that share `mail_bridge_mda`'s recipient);
      - the DNS-dependent perimeter gates cleared via the production
        `put_spam_policy` admin override (dnsbl=[], greylist off, fcrdns off,
        max_conn_per_min up) so a single loopback SMTP transaction completes;
      - in-process FakeClamd + FakeRspamd wired onto the MTA via the
        operator-hatch, so the T1.4 content-scan gate passes a benign body
        instead of 451ing all inbound (memory `mail-perimeter-scorer-ffi-shape`);
      - an MTA-role bridge spawned on the same domain (distinct bridge_id +
        keypair + self-sealed TLS cert; shared nest-global `mail_domains` row +
        alias + MLS pubkey — `_spawn_mta_bridge`).

    The MTA seals inbound DATA to the recipient's MSEK-derived pubkey; the MDA
    opens it with the same MSEK's snapshot leaf secret — so the test proves the
    MTA-sealed envelope round-trips through the MDA's `OpenMailRecord` read path.

    Partition with the deploy-verify track: that track owns the
    docker/compose full-stack round-trip; this is the fast (seconds), CI-friendly
    nest+bridge integration layer over the same seam.

    Process safety: owns only its spawned MTA subprocess (the MDA half is the
    `mail_bridge_mda` fixture's to clean up); tracks it through the same
    `_spawn_mta_bridge` atexit machinery, never pkill/killall.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd

    tmp = tmp_path_factory.mktemp("mail-inbound-to-imap")
    domain = mail_bridge_mda.domain
    nest_url = nest_instance["url"]

    # Fresh recipient on the MDA's domain → a clean INBOX. The MDA bridge already
    # running for this domain serves any recipient with a wrapped-MSEK blob +
    # snapshot, so no second IMAP bridge is needed — only the MTA half is added.
    recipient = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=domain,
        local_part="inbound-recipient",
        password="inbound-test-password-1",
    )

    # Clear the DNS-dependent perimeter gates (so port 25 binds 127.0.0.1 and a
    # single loopback transaction completes without DNSBL / greylist / fcrdns
    # stalls) via the production admin override — keeps the test tier_3.
    admin = nest_instance["admin"]
    admin_ws = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        admin_ws.call(
            "fauna.bridges.put_spam_policy",
            {
                "dnsbl_servers": [],
                "greylist_enabled": False,
                "greylist_delay_secs": 0,
                "fcrdns_mode": "off",
                "max_conn_per_min": 1000,
                "baseline_standing_publish": False,
            },
        )

    # Fake clamd + rspamd (benign verdicts) wired onto the MTA via the
    # operator-hatch — same posture as `mail_bridge_mta`. No `[mta_mx_override]`:
    # this seam is inbound-only (no outbound delivery), so no stub MX is needed.
    fake_clamd = FakeClamd().start()
    fake_rspamd = FakeRspamd().start()
    op_hatch_extra = (
        f'clamd_addr = "{fake_clamd.addr}"\n'
        f'rspamd_url = "{fake_rspamd.url}"\n'
    )

    spawned = _spawn_mta_bridge(
        mail_bridge_binary=mail_bridge_binary,
        seal_helper_binary=seal_helper_binary,
        nest_instance=nest_instance,
        tmp=tmp,
        bridge_id="test-inbound-mta-1",
        domain=domain,
        operator_hatch_extra=op_hatch_extra,
    )

    try:
        handle = MailBridgeInboundToImapHandle(
            mx_port=spawned.mx_port,
            imaps_port=mail_bridge_mda.imaps_port,
            domain=domain,
            recipient_username=recipient.username,
            recipient_password=recipient.password,
            mta_proc=spawned.proc,
            mta_log_file=spawned.log_file_path,
            mda=mail_bridge_mda,
            recipient=recipient,
        )
        yield handle
    finally:
        spawned.cleanup()
        fake_clamd.stop()
        fake_rspamd.stop()


class MailVenueHandle(MailVenueBridges):
    """The contract a **dedicated mail venue** answers, whatever runs its bridges.

    `MailVenueBridges` above is the two questions any mail venue answers about
    its bridge processes. This is the layer for a venue that owns a whole NEST as
    well: the `nest` dict and claimed `domain` every consumer reads, the admin
    wire call that opens the deployment gate, and — the one that made this class
    necessary — `rebind_after_enable`.

    **`rebind_after_enable` is not an absence in docker; it is a DIFFERENT ACT.**
    The standalone venue plays s6 because the binaries e2e has no supervisor: it
    waits for each bridge's exit-for-rebind and relaunches it. The image *is*
    supervised, so there the same call means "wait until the bridges are serving
    again" — the supervisor has already done the relaunching. Both answer the one
    question a test actually asks after driving an enable through the app UI (*is
    the subsystem up yet?*), which is why it belongs on the venue rather than in
    sixteen test bodies. Getting this wrong in either direction is the trap worth
    naming: a `None`/absence would strand every consumer, and a no-op would make
    them all pass vacuously.

    `testing.md` § Default app and nest mode, ruling (3) — the docker shape of a
    mail nest is the image's own s6 bridges enabled through the product's toggle.
    """

    #: Set by every subclass' `__init__`. Declared here so the two shared methods
    #: below can read them without either venue re-documenting the contract.
    nest: dict
    domain: str

    def admin_opens_mail_gate(self) -> None:
        """Flip the deployment-wide `mail_enabled` toggle ON as the nest ADMIN.

        **Only a test whose enabling actor is NOT this nest's admin needs this.**
        The usual consumer of a mail venue drives the app as the nest admin, so
        its own `enable_mail_plain` carries the deployment flip along with the
        mailbox provisioning: `MailSettingsMachine::enable_mail` calls
        `fauna.bridges.set_mail_enabled` best-effort
        (`libs/fauna-client-mail-settings/src/machine.rs`, "the non-admin
        no-op"). That kind is **Admin-class** on the nest
        (`bins/fauna-nest/src/bridge_method_allowlist.rs`), so when the app is
        driven as a NON-admin the call is rejected server-side and the client
        deliberately **swallows** the rejection — the user's mailbox is
        provisioned, but the deployment gate stays shut.

        A shut gate means `mta.Bindable` (`mail_enabled && len(local_domains) > 0`,
        `bins/fauna-bridges/internal/mta/mta.go`) never opens, so the idling
        bridge correctly never exits — and a `rebind_after_enable()` that follows
        will time out after 60s against a perfectly healthy bridge. That is a
        **test-setup** bug, not a bridge bug: in production the admin owns this
        toggle and turns the subsystem on for the deployment, while each user
        enables only their own mailbox (`mail-bridge-lifecycle.md` § Default-off
        on first claim).

        Call this after the non-admin's enable and BEFORE `rebind_after_enable()`.

        Mode-agnostic by construction: it is a wire call to the nest's own admin
        kind, so it is the same act in a container as against a local binary —
        which is exactly why it sits here rather than on either venue.
        """
        from clients.ws_rpc_admin_client import WsRpcAdminClient as _WsRpcAdminClient

        admin = self.nest["admin"]
        admin_ws = _WsRpcAdminClient(
            self.nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})

    def rebind_after_enable(self, *, mta=True, mda=True, timeout: float = 60.0) -> None:
        """Return once the bridges this enable affects are serving again.

        Pass `mta=False` / `mda=False` for a toggle that role does not gate on —
        enabling WebDAV rebinds only the MDA, since the MTA gates on mail alone.

        Latency-independent by construction (e2e-conventions.md convention 14):
        every implementation waits on an observable state change under a named
        generous budget, never on a delay.
        """
        raise NotImplementedError(
            f"{type(self).__name__} must answer rebind_after_enable — a mail "
            f"venue that cannot say when its bridges are serving again strands "
            f"every consumer that drives an enable through the app UI"
        )


class DedicatedMailNestHandle(MailVenueHandle):
    """A FRESH, isolated nest with MTA + MDA bridges on a claimed domain and
    **no recipient provisioned** — the client under test enables mail for this
    nest's own admin actor.

    Distinct from `mail_bridge_inbound_to_imap` (which pre-provisions a recipient
    server-side and shares the session `nest_instance`): here the recipient's
    mail key material is whatever the *client* provisions through `EnableMail`.
    A dedicated nest is required because the nest admin must be the only actor
    enabling mail there and must start with mail disabled — neither holds on the
    shared `nest_instance`, where other tests' users already share the box.

    Exposes the `nest` dict (admin keypair + url — the test injects this admin
    identity into the client), the claimed `domain`, the MTA `mx_port` (inbound
    port-25) + MDA `imaps_port`, and the submission machinery for the outbound
    leg: `submission_port_465` / `submission_port_587`, the in-process `stub_mx`
    the bridge relays external mail to (via the operator-hatch `mta_mx_override`),
    and the nest-published DKIM record `dkim_public_dns_value` / `dkim_selector`
    so an outbound-submission test can verify the relayed message's signature.
    """

    def __init__(
        self,
        *,
        nest,
        domain,
        mx_port,
        imaps_port,
        caldav_port,
        submission_port_465,
        submission_port_587,
        stub_mx,
        dkim_public_dns_value,
        dkim_selector,
        mta_proc,
        mta_log_file,
        mda_log_file=None,
        mda=None,
        mta=None,
    ):
        self.nest = nest
        self.domain = domain
        self.mx_port = mx_port
        self.imaps_port = imaps_port
        # MDA CalDAV HTTPS listener (self-signed local cert). Connect with
        # `verify=False`; the live example.com deployment serves a real ACME cert.
        # On the admin-port variant (`caldav_admin_port`) this is the admin-set
        # port the MDA binds via nest config (no hatch), not an ephemeral hatch port.
        self.caldav_port = caldav_port
        # The `_SpawnedMdaBridge` itself — exposed (vs. only `mda_proc`/`mda_log_file`)
        # so the admin-CalDAV-port rebind test can call `mda.respawn()` after the
        # MDA's config-driven exit. `None` on the canonical fixtures that never rebind.
        self.mda = mda
        self.submission_port_465 = submission_port_465
        self.submission_port_587 = submission_port_587
        self.stub_mx = stub_mx
        self.dkim_public_dns_value = dkim_public_dns_value
        self.dkim_selector = dkim_selector
        self.mta_log_file = mta_log_file
        # The MDA bridge's log (CalDAV REPORT / query_events / decrypt traces).
        self.mda_log_file = mda_log_file
        # The `_SpawnedMtaBridge` itself, for the same reason `mda` is exposed:
        # `rebind_after_enable` plays supervisor for both. `None` on the
        # CalDAV-only variant, which runs no MTA.
        self.mta = mta
        # `mta_proc` is deliberately NOT a stored field: `rebind_after_enable`
        # replaces the bridge's process, and a snapshot taken at fixture setup
        # would then point at the exited first boot — every
        # `handle.mta_proc.poll() is None` liveness assert in the consuming
        # tests would read a dead process and fail. Reading through keeps them
        # true across any number of rebinds. (`mta_proc` the constructor arg is
        # still accepted and ignored, for call-site compatibility.)

    @property
    def mta_proc(self):
        """This fixture's MTA process, live across rebinds. `None` on the
        CalDAV-only variant, which runs no MTA."""
        return self.mta.proc if self.mta is not None else None

    def rebind_after_enable(self, *, mta=True, mda=True, timeout: float = 60.0) -> None:
        """PLAY s6 after an enable driven through the app UI: wait for each
        affected bridge's exit-for-rebind, relaunch it, and return once it is
        serving.

        The standalone half of `MailVenueHandle.rebind_after_enable` — the venue
        that SPAWNED these bridges is the one that must restart them. (The docker
        venue answers the same question by waiting on its supervisor instead.)

        **Why a test must call this.** This fixture cold-boots its bridges with
        every deployment toggle OFF, which is exactly the state a freshly
        claimed nest is in (`mail-bridge-lifecycle.md` § Default-off on first
        claim: "A freshly-claimed nest ... has `mail.enabled = false`"), and its
        tests then turn mail/DAV on the way a user does — through the app. In
        production nest answers that toggle by signalling the s6 supervisor,
        which brings the bridge process up bound to the new shape; an idling
        bridge that is already running exits 0 for the same supervisor to
        restart it (`internal/wsrpc/idle_gate_watch.go`). The binaries e2e has
        no supervisor, so the fixture is it.

        Pass `mta=False` / `mda=False` for a toggle that role does not gate on —
        enabling WebDAV rebinds only the MDA, since the MTA gates on mail alone
        and therefore never exits.

        Latency-independent by construction (e2e-conventions.md convention 14):
        every step waits on an observable state change — process exit, then the
        listener accepting — under a named generous budget, never on a delay.
        """
        if mta:
            if self.mta is None:
                raise RuntimeError(
                    "rebind_after_enable(mta=True) on a fixture with no MTA "
                    "(the caldav_only variant runs none) — pass mta=False"
                )
            self.mta.rebind(timeout=timeout)
        if mda:
            self.mda.rebind(timeout=timeout)


class DockerMailVenueHandle(MailVenueHandle):
    """A dedicated mail venue whose bridges are **the image's own s6 services**.

    The docker answer to `DedicatedMailNestHandle`, and the whole of arm 6's
    venue: `testing.md` § Default app and nest mode, ruling (3) settles that *a
    host-spawned bridge is not a knob* — "the docker shape of a mail nest is the
    image's own s6 bridges enabled through the product's toggle, its mail ports
    published to free host ports, the venue's sidecars beside it."

    So the port facts come from the PUBLICATION (known at container start, which
    is exactly why the venue is a provider method and not a start option), and
    the two bridge questions are answered by the supervisor rather than by a
    `subprocess.Popen` the harness holds.

    **What is deliberately absent, and why each is a fact rather than a gap.**
    Ruling (3) names the host-spawn affordances as this mode's declared
    absences: `mda` (for `respawn()` — s6 owns the rebind in the image, which is
    why the admin-CalDAV-port rebind test stays standalone-only), the MTA/MDA log
    FILES (there is no host path; the output went to the container), and the
    metrics port. Two more are absences of the same kind, found by measuring what
    consumers read: `stub_mx` (an in-process SMTP sink the harness spawns and the
    operator-hatch `mta_mx_override` routes to — the image's bridges read their
    own config, so the docker answer is a fake-DNS sidecar, a different mechanism
    rather than a different value) and the DKIM pair `dkim_selector` /
    `dkim_public_dns_value` (the standalone fixture reads them from the nest).
    None of them is *silently* absent: they are simply not attributes here, so a
    test reading one gets an `AttributeError` naming this class — and, before it
    could ever get that far, a collection-time exclusion, since the four test
    functions that read them are pinned in `helpers/nest_surface.py`'s
    `MAIL_VENUE_HOST_AFFORDANCE_READERS` and excluded from this mode by name.
    """

    def __init__(self, *, nest, domain, mail_ports, container_name):
        self.nest = nest
        self.domain = domain
        #: container_port -> published host port, as handed to `docker run`.
        self._mail_ports = dict(mail_ports)
        self.container_name = container_name
        #: **The venue's SHAPE, read off its own publication rather than passed
        #: beside it.** A CalDAV-only deployment never writes
        #: `/data/imap-enabled`, so the image's MTA re-downs itself by its own
        #: run-script gate and its three SMTP listeners never bind — which is
        #: why that venue publishes no SMTP port, and why "no port 25 published"
        #: and "this venue runs no MTA" are one fact rather than two that can
        #: disagree. Deriving it makes an inconsistent handle unrepresentable.
        self.caldav_only = 25 not in self._mail_ports
        if not self.caldav_only:
            self.mx_port = mail_ports[25]
            self.submission_port_465 = mail_ports[465]
            self.submission_port_587 = mail_ports[587]
            # ⚠ Absent on the CalDAV-only venue, and NOT a parity gap with
            # standalone — which does expose one there. `_spawn_mda_bridge(
            # pin_caldav_hatch=True)` pins the IMAP listeners into the operator
            # hatch whatever the deployment toggle says, so standalone's
            # CalDAV-only MDA binds IMAPS anyway: the same host-spawn affordance
            # hiding in plain sight that the CalDAV listener turned out to be.
            # The image has no hatch and binds each protocol per its own
            # `fetch_config` flag, so with mail off there is genuinely nothing
            # listening. Attribute-absent rather than `None`, per this class's
            # rule for every other absence.
            self.imaps_port = mail_ports[993]
        # 8443 is the MDA's DEFAULT CalDAV port and, since the nest itself moved
        # to 3000, it is CalDAV-only on every interface — so publishing it is the
        # whole of `caldav_port` here, with no `set_caldav_port` to arrange
        # (`tests/platform/docker/test_caldav_bare_ip_serving.py` is the proof).
        self.caldav_port = mail_ports[8443]

    # ── the two MailVenueBridges questions, answered by the supervisor ──

    def assert_mta_running(self) -> None:
        """Precondition: this venue's MTA has not died on us.

        The standalone venue asks `proc.poll() is not None` of a bridge it
        spawned. The faithful translation is NOT "is the service up": in the
        image the MTA is legitimately **down** until the admin's enable writes
        `/data/imap-enabled`, and every consumer of this venue calls this
        BEFORE driving that enable. Down-with-exit-0 is the idle state the s6
        run-script itself chose (`exec s6-svc -d` when no enable flag exists);
        down with a NON-zero exit is the crash the standalone assert exists to
        catch. So that is what this reads.
        """
        from tests.platform.docker.helpers import svstat

        from tests.platform.docker.helpers import is_commanded_up

        if self.caldav_only:
            # The same answer `MailVenueBridges` gives for standalone's
            # CalDAV-only variant, and it must be the same answer: the two
            # venues answer ONE contract, so a consumer must not have to know
            # which one it is holding. Refusing here rather than reading the
            # supervisor is also the only *correct* reading — the image's MTA
            # service exists and reports its ordinary `down ... normally up`
            # idle state, so the supervisor check below would PASS and tell a
            # test that requires an MTA that it has one.
            raise AssertionError(
                "this mail venue runs no MTA bridge (the CalDAV-only variant "
                "publishes no SMTP port and never writes /data/imap-enabled, so "
                "the image's MTA service stays down by its own s6 run-script "
                "gate), so a test that requires one cannot run against it"
            )
        st = svstat(self.container_name, "fauna-mail-bridge-mta")
        if not st:
            raise AssertionError(
                f"the MTA service is not known to this container's supervisor "
                f"(s6-svstat said nothing for fauna-mail-bridge-mta in "
                f"{self.container_name!r}); {self.bridge_log_hint('mta')}"
            )
        # **The discriminator is WANT, not exit status**, and getting that wrong
        # was this venue's own first bug (measured 2026-09-02): a first cut read
        # a non-`exitcode 0` down as a crash, and the image's ordinary idle state
        # reports `down (signal SIGTERM) 1 seconds, normally up, ready` — because
        # the deliberate idle-down is the run-script `exec s6-svc -d`-ing itself,
        # which the supervisor performs by signalling. So exit status cannot tell
        # "deliberately off" from "died", and the check false-failed every test
        # against a perfectly healthy container.
        #
        # What DOES separate them is the supervisor's intent: a service the
        # supervisor wants up and that is nonetheless down is a crash or a
        # restart loop — the exact thing standalone's `proc.poll() is not None`
        # catches — while a service that is down because nothing has asked for it
        # is the correct pre-enable state of an image whose MTA gates on
        # `/data/imap-enabled`. `is_commanded_up` already draws that line for the
        # bring-up helpers; this is the same predicate read for the same fact.
        if "want up" in st and not st.startswith("up "):
            raise AssertionError(
                f"the supervisor wants the MTA bridge up and it is not "
                f"(s6-svstat: {st!r}) — a crash or a restart loop, not the "
                f"idle-down state of a nest whose mail is still off; "
                f"{self.bridge_log_hint('mta')}"
            )
        assert is_commanded_up(st) or st.startswith("down"), (
            f"unrecognised s6 state for the MTA bridge: {st!r}; "
            f"{self.bridge_log_hint('mta')}"
        )

    def bridge_log_hint(self, role: str = "mta") -> str:
        """Where this venue's `role` bridge output went — a `docker logs` line.

        The container multiplexes every s6 service onto its own stdout, so there
        is no per-role file to point at and the honest answer names the container
        rather than pretending otherwise.
        """
        assert role in ("mta", "mda"), f"unknown bridge role {role!r}"
        return (f"{role} log: docker logs {self.container_name} "
                f"(all s6 services share the container's stdout)")

    def bridge_log_lines(self, role: str = "mta") -> list[str]:
        """This venue's container log, as lines, newest last.

        Never raises — a diagnostic helper runs inside a failure path, where
        raising would replace the assertion that actually failed.
        """
        assert role in ("mta", "mda"), f"unknown bridge role {role!r}"
        import subprocess as _sp

        try:
            out = _sp.run(["docker", "logs", "--tail", "400", self.container_name],
                          capture_output=True, text=True, timeout=20)
        except Exception:
            return []
        return [ln.rstrip() for ln in (out.stdout + out.stderr).splitlines()]

    # ── the venue question: is the subsystem up after the app's enable? ──

    def rebind_after_enable(self, *, mta=True, mda=True, timeout: float = 60.0) -> None:
        """Wait until the image's supervised bridges are serving again.

        The DIFFERENT ACT `MailVenueHandle` names. Standalone plays s6 because
        the binaries e2e has no supervisor; here the supervisor has already
        relaunched the services on the enable flag, so the venue's job is the
        convergence wait — the same one `bring_bridges_to_serving` does after its
        own `set_mail_enabled`, which is exactly why that function's post-enable
        half was split out as `await_bridges_serving`. The app under test fired
        the enable through its mail-settings page; re-firing it here would mask a
        client that never did.

        `mta=False` (a CalDAV/WebDAV-only enable) waits on the MDA alone, and
        deliberately does NOT wait for the IMAPS handshake: the MDA binds each
        protocol per its own `fetch_config` flag, so with mail still off there is
        no IMAPS listener to wait for and doing so would hang for the budget.
        """
        from tests.platform.docker.helpers import (
            await_bridges_serving, wait_for_tls_handshake,
        )

        if mta and self.caldav_only:
            # The standalone venue's own refusal, word for word in substance
            # (`DedicatedMailNestHandle.rebind_after_enable`), because the two
            # venues answer one contract. Refusing beats letting the mail branch
            # run: it would block on an SMTP banner from a listener that will
            # never bind and fail after the whole budget with a timeout naming a
            # port, when the real diagnosis is the venue's shape.
            raise RuntimeError(
                "rebind_after_enable(mta=True) on a venue with no MTA (the "
                "caldav_only variant publishes no SMTP port and the image's MTA "
                "stays down without /data/imap-enabled) — pass mta=False"
            )
        if mta and mda:
            await_bridges_serving(self.container_name, self.nest,
                                  self._mail_ports, self.domain)
            # `await_bridges_serving` waits on the four MAIL listeners (25/587
            # banners, 465/993 handshakes) and stops there — it was written for
            # the mail round-trip. This venue also promises CalDAV, and its
            # listener is a fifth published port that comes up on the same cert
            # fan-out, so waiting for it here is what makes `caldav_port` usable
            # the instant this returns. Without it a CalDAV consumer would race
            # the fan-out and see an `SSLError` from a listener that was about to
            # be fine.
            wait_for_tls_handshake(
                "127.0.0.1", self.caldav_port,
                expect_banner_prefixes=None, timeout=timeout)
            return
        if not mda:
            raise RuntimeError(
                "rebind_after_enable(mda=False) is not a shape this venue has: "
                "the image runs ONE MDA process hosting IMAP + CalDAV + CardDAV "
                "+ WebDAV, so there is no MTA-only rebind to wait for"
            )
        self._await_mda_serving(timeout=timeout)

    def _await_mda_serving(self, *, timeout: float = 60.0) -> None:
        """Wait until the supervised MDA is serving its DAV listener.

        Split out of `rebind_after_enable` because the CalDAV-only venue needs
        the identical convergence at its own SETUP rather than after an app's
        enable — the same reason `await_bridges_serving` was split out of
        `bring_bridges_to_serving`, one layer down. There the venue promises
        mail + CalDAV and the app under test drives the enable; here the venue
        promises a CalDAV-only deployment and therefore drives `set_caldav_
        enabled` itself, so the wait has no enable of its own to follow.

        Standalone's CalDAV-only venue hands its fixture back with the MDA
        already spawned and its listener accepting (`_spawn_mda_bridge` waits),
        so a docker venue that yielded before convergence would not be the same
        contract — every consumer would race the cert fan-out.
        """
        from tests.platform.docker.helpers import (
            await_approved_bridges, await_keypairs, admin_ws, bridge_diag,
            flag_present, is_commanded_up, provision_self_signed_cert,
            restart_service, svstat, wait_for_tls_handshake,
        )
        import time as _time

        # MDA-only: a CalDAV / CardDAV / WebDAV enable. The MDA's s6 run-script
        # re-downs itself only when NONE of /data/{imap,caldav,carddav,webdav}
        # -enabled exists, so the flag to wait on is whichever one the app just
        # wrote — poll for any of them rather than assuming the mail one.
        deadline = _time.monotonic() + timeout
        flags = ("imap-enabled", "caldav-enabled", "carddav-enabled",
                 "webdav-enabled")
        while _time.monotonic() < deadline:
            if (any(flag_present(self.container_name, f) for f in flags)
                    and is_commanded_up(
                        svstat(self.container_name, "fauna-mail-bridge-mda"))):
                break
            _time.sleep(0.5)
        present = [f for f in flags if flag_present(self.container_name, f)]
        assert present, (
            f"no /data/*-enabled flag materialized within {timeout}s, so the s6 "
            f"MDA stayed down: the enable driven through the app never reached "
            f"the nest. {self.bridge_log_hint('mda')}"
        )
        st = svstat(self.container_name, "fauna-mail-bridge-mda")
        assert is_commanded_up(st), (
            f"MDA must be commanded up once {present} exists; got {st!r}. "
            f"{self.bridge_log_hint('mda')}"
        )
        try:
            await_keypairs(self.container_name, ("mda",))
            with admin_ws(self.nest) as admin:
                assert "mda" in await_approved_bridges(admin, ("mda",)), (
                    "the MDA must reach approved (auto-approve on enable)")
            restart_service(self.container_name, "fauna-mail-bridge-mda")
            deadline = _time.monotonic() + timeout
            sealed: set = set()
            while _time.monotonic() < deadline:
                reply = provision_self_signed_cert(self.nest, self.domain)
                sealed = {b["role"] for b in reply.get("bridges_sealed_to", [])}
                if "mda" in sealed:
                    break
                _time.sleep(3.0)
            assert "mda" in sealed, (
                f"cert must fan out sealed to the mda (proves x25519 "
                f"attestation); last sealed={sealed}")
            # `None`, NOT `()`: the helper reads the sentinel as "handshake
            # only, no banner", while an empty tuple would make its `any(...)`
            # test unsatisfiable and time out against a perfectly live listener.
            # CalDAV speaks HTTPS, so there is no banner to read — the completed
            # handshake IS the proof the cert reached the bridge.
            wait_for_tls_handshake(
                "127.0.0.1", self.caldav_port,
                expect_banner_prefixes=None, timeout=timeout)
        except (AssertionError, TimeoutError) as e:
            raise AssertionError(
                f"{e}\n\n── bridge diagnostics ──\n"
                f"{bridge_diag(self.container_name, ('mda',))}") from e


def _dedicated_mail_nest_impl(
    mail_bridge_binary, seal_helper_binary, nest_binary, tmp_path_factory,
    *, label="dedicated-mail-nest", registration_open=False,
    caldav_only=False, caldav_admin_port=None,
):
    """Stand up a dedicated nest + MTA + MDA on a claimed domain, recipient NOT
    provisioned (the client enables mail for the nest admin).

    Parameterized generator shared by the ``dedicated_mail_nest`` fixture (the
    canonical claimed mail+CalDAV tier_3 nest) and ``dedicated_caldav_mailbox_less_nest``
    (the same, but with open self-service
    registration so a test can seed an addressable, mailbox-less Fauna attendee).
    The two fixtures differ only in those nest-creation knobs; everything else —
    domain claim, spam-policy clear, stub MX, MTA + MDA spawn — is identical,
    so it lives here once (priority #2/#4: one spawn path, not a copy).

    ``caldav_admin_port`` (default ``None``) builds the **admin-CalDAV-port**
    variant: the admin sets ``fauna.bridges.set_caldav_port`` to that port BEFORE
    the MDA boots, and the MDA is spawned WITHOUT a ``caldav_listen_https`` hatch
    (``pin_caldav_hatch=False``) so it binds the admin port from nest config and
    arms the rebind-on-``config_changed`` exit. The handle's ``caldav_port`` is
    then that admin port and ``handle.mda`` carries the ``_SpawnedMdaBridge`` so a
    test can ``respawn()`` it after changing the port. Only meaningful with
    ``caldav_only=True`` (the admin-port acceptance test's topology).

    Function-scoped (a fresh nest per test) because the bridge hardcodes the
    AUTH credential_id to `"default"` (imap/auth.go § plainCredentialID), so an
    admin holds exactly one auth-reachable credential — the PLAIN and OAUTHBEARER
    round-trips would collide on that one slot if they shared a nest. Each test's
    admin must start with mail disabled and own the sole `default` credential it
    mints, which a fresh nest guarantees.

    Mirrors `mail_bridge_inbound_to_imap`'s deployment setup (a claimed primary
    domain, DNS-perimeter gates cleared, fake clamd/rspamd, one MTA + one MDA
    bridge) but on a fresh nest and WITHOUT `_provision_msek_recipient` — the
    recipient pubkey + wrapped-MSEK + snapshot are the client's to provision
    via the enable-mail UI. See `DedicatedMailNestHandle`.

    Process safety: owns its nest subprocess + both bridge subprocesses; tears
    them down in reverse order on teardown, never pkill/killall.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd

    # No domain start option — see the `add_local_domain` below, which is this
    # fixture's own registration of MAIL_PRIMARY_DOMAIN and therefore the act
    # that sets the deployment identity (`apply_primary_identity`). Callers used
    # to pass `handle_domain=MAIL_PRIMARY_DOMAIN` to get an identity the very
    # next block was about to establish anyway; the four that did are unchanged
    # in behaviour and one option lighter (`testing.md` § Default app and nest
    # mode, ruling (3)).
    nest, nest_cleanup = _make_nest(nest_binary, tmp_path_factory, label)
    if registration_open:
        # Post-boot, over the admin kind — the same wire call the block below
        # makes for this fixture's other deployment choices (mail domain, spam
        # policy, the CalDAV toggles). The registration posture stopped being a
        # nest-start knob in arm 4; it is an admin choice like the rest of them.
        from common.auth import open_registration
        open_registration(nest)
    # Each bridge gets its OWN tmp subdir — the same root fix the
    # `caldav-variant-*` fixture below already applies, for the same reason.
    # `_spawn_mta_bridge` and `_spawn_mda_bridge` both write
    # `operator-hatch.toml` and both use `data_dir = tmp`, so co-locating them in
    # one `tmp` makes the second spawn (the MTA, below) CLOBBER the first's
    # (the MDA's) hatch.
    #
    # ⚠ This was documented as "benign in `dedicated_mail_nest` (both
    # pre_approve + bind eagerly at spawn, before the clobber)" — and it WAS,
    # until 2026-08-21 gave every general-path consumer a
    # `rebind_after_enable()`. A respawn re-reads the hatch from disk, and by
    # then the MDA's has been overwritten by the MTA's — which carries no
    # `imap_listen_*` / `caldav_listen_https` lines at all, so the respawned MDA
    # fell back to the privileged defaults (`:993` / `:143` / `:8443`) and to the
    # MTA's `metrics_bind_addr`, whose port was already held by the live MTA. Its
    # `/healthz` never came up on the port the fixture recorded, and
    # `rebind_after_enable()` failed 60s later for every consumer of this
    # fixture. Separate dirs keep each hatch + data_dir private.
    _tmp_base = tmp_path_factory.mktemp("dedicated-mail-bridges")
    tmp_mta = _tmp_base / "mta"
    tmp_mta.mkdir()
    tmp_mda = _tmp_base / "mda"
    tmp_mda.mkdir()
    domain = MAIL_PRIMARY_DOMAIN
    admin = nest["admin"]

    # Claim the primary domain + clear the DNS-dependent perimeter gates so a
    # single loopback SMTP transaction completes (same overrides as
    # `mail_bridge_inbound_to_imap`).
    admin_ws = WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        admin_ws.call(
            "fauna.bridges.add_local_domain",
            {"domain": domain, "mta_sts_cert_mode": "per_host"},
        )
        admin_ws.call(
            "fauna.bridges.put_spam_policy",
            {
                "dnsbl_servers": [],
                "greylist_enabled": False,
                "greylist_delay_secs": 0,
                "fcrdns_mode": "off",
                "max_conn_per_min": 1000,
                "baseline_standing_publish": False,
            },
        )
        if caldav_only:
            # CalDAV-ONLY deployment: set the deployment toggles BEFORE the MDA
            # boots — CalDAV ON, email explicitly OFF. Post-Stage-5 an *unset*
            # `mail_enabled` already reads OFF, so the explicit false is now
            # redundancy — kept because explicit-off states the deployment's
            # intent (and pins the CalDAV toggle's independence from mail: an
            # unset caldav_enabled would follow mail off). The MDA binds its
            # listener set once at startup and exits on a later gating-tuple
            # flip (mda.go relies on s6 to rebind, which the e2e lacks), so
            # both toggles must be set pre-boot for it to come up bound to
            # CalDAV with mail off — the email-disabled nest the auto-schedule
            # classifier routes local Fauna attendees onto the sealed
            # scheduling rail from (caldav-server.md § Server-side
            # auto-schedule — email-disabled nest → WS-RPC sealed delivery).
            admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": False})
            admin_ws.call("fauna.bridges.set_caldav_enabled", {"enabled": True})
        if caldav_admin_port is not None:
            # Set the admin CalDAV port BEFORE the MDA boots so its first
            # `fetch_config` reads it and binds it (no hatch → admin port governs,
            # `resolveMDAListenAddrs` → `:<port>`). Admin-gated `fauna.bridges.*`
            # RPC, mirroring `set_caldav_enabled` above.
            admin_ws.call("fauna.bridges.set_caldav_port", {"port": caldav_admin_port})

    fake_clamd = FakeClamd().start()
    fake_rspamd = FakeRspamd().start()

    # Stub external MX for the outbound-submission leg's delivery. Started
    # before the operator-hatch is written so its ephemeral port wires into
    # `mta_mx_override`. The client provisions the submission token + sender
    # MLS pubkey via EnableMail (so unlike `mail_bridge_mta`, this fixture
    # provisions no server-side sender identity); the only nest-global state
    # the outbound leg needs beyond inbound is the routing override (the nest
    # already holds the domain's DKIM key), so an outbound test exercises the *client-minted* credential end-to-end.
    from helpers.stub_mx import StubMX
    stub_mx = StubMX().start()
    op_hatch_extra = (
        f'clamd_addr = "{fake_clamd.addr}"\n'
        f'rspamd_url = "{fake_rspamd.url}"\n'
        f'\n[mta_mx_override]\n'
        f'"external.test" = "{stub_mx.target}"\n'
        # RFC 2606 reserved example domain — routed to the stub MX so a test can
        # send to `test@example.com` (the user's literal 2026-06-04 scenario) and
        # prove it relays out, fully hermetic (no real DNS / no real delivery).
        f'"example.com" = "{stub_mx.target}"\n'
        f'"{domain}" = "{stub_mx.target}"\n'
    )

    mda = _spawn_mda_bridge(
        mail_bridge_binary=mail_bridge_binary,
        nest_instance=nest,
        tmp=tmp_mda,
        bridge_id="dedicated-mda-1",
        domain=domain,
        # Admin-port variant: no CalDAV hatch, so the MDA binds the admin-set
        # `caldav_port` and arms the rebind-on-config-change exit.
        pin_caldav_hatch=caldav_admin_port is None,
    )
    # A CalDAV-only deployment runs NO MTA: with mail off, `mta.Run` idles and
    # never binds its SMTP listener, so `_spawn_mta_bridge`'s listener-wait would
    # time out. Skip it and leave the MTA-only handle fields unset.
    mta = None
    if not caldav_only:
        mta = _spawn_mta_bridge(
            mail_bridge_binary=mail_bridge_binary,
            seal_helper_binary=seal_helper_binary,
            nest_instance=nest,
            tmp=tmp_mta,
            bridge_id="dedicated-mta-1",
            domain=domain,
            operator_hatch_extra=op_hatch_extra,
            # This fixture's whole point is that mail starts OFF and the CLIENT
            # turns it on, so at spawn `mta.Run` idles and binds no SMTP
            # listener — there is nothing to wait for yet. Waiting here is what
            # made every consuming test die at fixture SETUP once the 2026-08-14
            # Stage-5 flip made an unset `mail_enabled` read OFF: the
            # bind decision had already been made, permanently, before the wait
            # even started, so it could only ever time out. A test drives its
            # enable and then calls `handle.rebind_after_enable()`, which waits
            # for the listener that the enable actually causes.
            wait_for_serving=False,
        )

    try:
        yield DedicatedMailNestHandle(
            nest=nest,
            domain=domain,
            mx_port=mta.mx_port if mta else None,
            imaps_port=mda.imaps_port,
            # The admin-port variant binds the admin-set port (the MDA's own
            # `caldav_port` is `None` then — no hatch); fall back to the hatch
            # port for the canonical fixtures.
            caldav_port=caldav_admin_port if caldav_admin_port is not None else mda.caldav_port,
            submission_port_465=mta.submission_port_465 if mta else None,
            submission_port_587=mta.submission_port_587 if mta else None,
            stub_mx=stub_mx,
            # The nest minted the domain's DKIM key at `add_local_domain`
            # above; this is the record it publishes for it.
            dkim_public_dns_value=_published_dkim_record(nest, domain) if mta else None,
            dkim_selector="default" if mta else None,
            mta_proc=mta.proc if mta else None,
            mta_log_file=mta.log_file_path if mta else None,
            mda_log_file=mda.log_file_path,
            mda=mda,
            mta=mta,
        )
    finally:
        if mta:
            mta.cleanup()
        mda.cleanup()
        stub_mx.stop()
        fake_clamd.stop()
        fake_rspamd.stop()
        nest_cleanup()


@pytest.fixture(scope="function")
def dedicated_mail_nest(request, nest_mode, tmp_path_factory):
    """The canonical claimed-nest + MTA + MDA tier_3 fixture (mail + CalDAV).

    See :func:`_dedicated_mail_nest_impl`. Default knobs
    (``registration_open=False``) — behavior unchanged from before the
    parameterized extraction. Neither of the impl's former nest-start knobs is
    one any more: ``registration_open`` is applied post-boot over the admin kind,
    and the handle domain comes from the impl's own ``add_local_domain`` (arm 4).

    **Mode-routed** (arm 6): the venue is the provider's, so in docker this is
    the image's own s6-supervised bridges with their mail ports published rather
    than two host processes beside a binary. Same handle contract either way —
    `MailVenueHandle`.
    """
    handle, cleanup = _start_mail_venue(
        request, nest_mode, tmp_path_factory, "dedicated-mail-nest")
    try:
        yield handle
    finally:
        cleanup()


@pytest.fixture(scope="function")
def dedicated_mail_nest_handle_domain(request, nest_mode, tmp_path_factory):
    """``dedicated_mail_nest`` under a distinct label, kept because the tui
    outbound-From replyability test wants its own nest.

    The nest's handle domain IS the mail domain (``fauna.test``), so the claimed
    admin's whoami handle domain (``account_core::handle_domain``) equals the
    deployment mail domain and a client's outbound ``From:`` —
    ``<handle>@<handle-domain>``, the whoami domain per
    ``client-outbound-from-uses-handle-domain`` — is the routable mail address.

    ⚠ That is now true of ``dedicated_mail_nest`` as well, and this docstring
    used to say otherwise. Both fixtures register ``fauna.test`` as the primary
    mail domain, and registering the primary IS setting the deployment identity
    (``apply_primary_identity``) — a gap closed in the nest, and pinned there by
    ``add_local_domain_creates_primary_then_lists``. The old
    ``handle_domain=fauna.test`` start option this fixture passed was asking for
    what its own registration was about to do anyway, which is why arm 4 could
    delete it with no behaviour change. What remains between the two fixtures is
    the label and the separate nest; everything else (MTA + MDA + stub MX +
    ``mta_mx_override`` for ``external.test``) mirrors ``dedicated_mail_nest``.

    **Mode-routed** (arm 6) — see ``dedicated_mail_nest``."""
    handle, cleanup = _start_mail_venue(
        request, nest_mode, tmp_path_factory, "dedicated-mail-nest-handle-domain")
    try:
        yield handle
    finally:
        cleanup()


@pytest.fixture(scope="function")
def dedicated_caldav_mailbox_less_nest(request, nest_mode, tmp_path_factory):
    """A claimed mail+CalDAV nest whose handle domain IS the mail domain
    (``fauna.test``) and which opens self-service registration — so a test can
    seed a **mailbox-less** Fauna attendee: a handled actor (``register_handled_actor``
    → ``create_user_with_handle``) with published key packages but NO mail alias.

    This is the topology the MDA server-side auto-schedule **sealed rail** needs
    (caldav-server.md § Server-side auto-schedule): the organizer (the claimed
    admin, mail-enabled) PUTs an invite to the attendee ``<handle>@fauna.test``;
    the MDA classifies that local address — ``resolve_recipient`` Reject (no
    alias) → ``fauna.actor.by_handle`` resolves the handle → ``keypackage.fetch``
    Some → the sealed scheduling rail. The impl's own registration of
    ``fauna.test`` is what makes the attendee's mailto a LocalDomain AND lets
    ``register_handled_actor`` sign a matching domain — registering the primary
    domain IS setting the deployment identity, so no start option is needed for
    it; everything else mirrors ``dedicated_mail_nest``.

    **Mode-routed** (arm 6) — see ``dedicated_mail_nest``.
    """
    handle, cleanup = _start_mail_venue(
        request, nest_mode, tmp_path_factory, "dedicated-caldav-mbl-nest",
        registration_open=True)
    try:
        yield handle
    finally:
        cleanup()


@pytest.fixture(scope="function")
def dedicated_caldav_only_nest(request, nest_mode, tmp_path_factory):
    """A **CalDAV-only** tier_3 nest: the deployment CalDAV toggle is enabled
    before the MDA boots, and email is **never** enabled (``mail_enabled`` stays
    false). The MDA comes up bound to CalDAV only, so the server-side
    auto-schedule classifier sees an **email-disabled nest** and routes a local
    Fauna attendee onto the WS-RPC **sealed scheduling rail** rather than the
    email rail (caldav-server.md § Server-side auto-schedule — email-disabled nest
    → sealed delivery; the same-nest mirror of the cross-nest
    ``resolve_attendee_transport`` keying on the peer's ``email_enabled``).

    This is the realistic mailbox-less topology a mail-enabled nest can't model:
    on a mail-enabled nest every CalDAV-enabled actor's canonical
    ``<handle>@<domain>`` alias resolves (``ensure_canonical_handle_alias`` fires
    on the recipient-pubkey provision regardless of email), so the classifier
    would send them to the email rail. The venue's own ``fauna.test``
    registration (which is also what sets the identity) + ``registration_open``
    so a test can seed a handled Fauna attendee.

    **Mode-routed** (arm 6) — see ``dedicated_mail_nest``. The two modes reach
    the same CalDAV-only deployment by opposite roads, and the ONE difference is
    who supplies the bridge: standalone spawns an MDA whose operator hatch pins
    a CalDAV listener, while docker publishes 8443 and turns the product's own
    CalDAV toggle on, letting the image's s6 supervisor bring the MDA up
    (``docker/s6/fauna-mail-bridge-mda/run`` gates on ``/data/caldav-enabled``).
    Neither mode runs an MTA: standalone skips the spawn, and the image's MTA
    re-downs itself without ``/data/imap-enabled``.
    """
    handle, cleanup = _start_mail_venue(
        request, nest_mode, tmp_path_factory, "dedicated-caldav-only-nest",
        registration_open=True, caldav_only=True)
    try:
        yield handle
    finally:
        cleanup()


@pytest.fixture(scope="function")
def dedicated_caldav_admin_port_nest(
    mail_bridge_binary, seal_helper_binary, nest_binary, tmp_path_factory
):
    """A **CalDAV-only** tier_3 nest whose MDA binds an **admin-set** CalDAV port
    (no operator-hatch), for the admin-CalDAV-port rebind acceptance test.

    Like :func:`dedicated_caldav_only_nest` but the admin sets
    ``fauna.bridges.set_caldav_port`` to a free port BEFORE the MDA boots and the
    MDA is spawned WITHOUT a ``caldav_listen_https`` hatch, so it binds that admin
    port (``resolveMDAListenAddrs`` → ``:<port>``) and arms
    ``caldavPortRebindNeeded`` (``CalDAVBindIsAdminPort``). ``handle.caldav_port``
    is the admin port and ``handle.mda`` carries the ``_SpawnedMdaBridge`` so the
    test can change the port over admin WS-RPC and ``handle.mda.respawn()`` the
    rebind (the binaries e2e has no s6 supervisor). This is the headless,
    on-principle counterpart to the production flow where an admin picks the
    CalDAV port from the client UI (caldav-server.md § Network exposure)."""
    from drivers.port_util import find_free_port

    yield from _dedicated_mail_nest_impl(
        mail_bridge_binary, seal_helper_binary, nest_binary, tmp_path_factory,
        label="dedicated-caldav-admin-port-nest",
        registration_open=True,
        caldav_only=True, caldav_admin_port=find_free_port(),
    )


class UnclaimedMailNestUiHandle(MailVenueBridges):
    """Handle for `unclaimed_mail_nest_ui` — a fresh, never-claimed nest with an
    UNAPPROVED Go MTA bridge + a stub external MX, for the full client-UI
    single-user mail round-trip (tracked internally, Slice 2).

    ⚠ Mixes in `MailVenueBridges` like every other mail-venue handle. It did not
    until 2026-09-12, and the gap was invisible until a failure path asked: five
    linux tests in the 2026-09-11 sweep reported
    `AttributeError: 'UnclaimedMailNestUiHandle' object has no attribute
    'bridge_log_hint'` INSTEAD of whatever they had actually failed on — the exact trap `bridge_log_hint`'s own docstring
    was written to close on the other handles, reopened here by a class that
    carried `mta_log_file` but not the method that reads it. A diagnostic helper
    runs inside an `except`, so a missing one does not merely fail to help: it
    destroys the evidence.
    """

    def __init__(self, *, nest, domain, claim_code, mx_port, metrics_port,
                 stub_mx, bridge_ed_pubkey_hex, bridge_id, tls_blob,
                 mta_log_file, mta_proc=None):
        self.nest = nest
        self.nest_url = nest["url"]
        self.domain = domain
        self.claim_code = claim_code
        self.mx_port = mx_port
        self.metrics_port = metrics_port
        self.stub_mx = stub_mx
        self.bridge_ed_pubkey_hex = bridge_ed_pubkey_hex
        self.bridge_id = bridge_id
        self.tls_blob = tls_blob
        self.mta_log_file = mta_log_file
        # `assert_mta_running`'s subject. The venue DOES run an MTA (unapproved,
        # binding no listeners until the UI approves it), so the honest answer is
        # the process, not `None`.
        self.mta_proc = mta_proc


@pytest.fixture
def unclaimed_mail_nest_ui(
    mail_bridge_binary, seal_helper_binary, nest_binary, tmp_path_factory
):
    """A FRESH UNCLAIMED nest + an UNAPPROVED Go MTA bridge + a stub external MX,
    for the full client-UI single-user mail round-trip (tracked internally,
    Slice 2).

    Nothing admin-side is provisioned here — the nest has no admin yet. The test
    claims the nest through the client UI (becoming admin), then drives
    add-domain + bridge-approval + enable-mail through the UI, using the accepted
    scaffolding (the returned sealed `tls_blob`, a `put_spam_policy` to clear the
    DNS-perimeter gates) only where there is no client surface — you cannot get a
    real cert/DNS for a test domain.

    The MTA is spawned UNAPPROVED (`pre_approve=False`, `wait_for_serving=False`):
    it self-enrolls (`request_enrollment` → a PENDING row) and binds no listeners
    until the test approves it via the client's admin-bridges-pending page, at
    which point it cold-boots fully and serves on `mx_port` reading the by-then-
    registered domain (this ordering sidesteps the boot-domain-less hazard).

    Process safety: owns its nest + bridge subprocesses + the in-process stubs;
    tears them down on teardown, never pkill/killall.
    """
    from common.nest import CLAIM_CODE
    from helpers.stub_mx import StubMX
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd

    # Pin the nest's handle domain to the mail domain so a claimed actor's
    # whoami reports `<handle>@fauna.test` — the client then stamps that as its
    # outbound mail `From`, which IS the routable canonical alias enable_mail
    # creates. Mirrors a real deploy (node domain == handle domain == mail
    # domain); without it whoami falls back to the node domain (`localhost`) and
    # the client sends from a non-mail, non-replyable address.
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "unclaimed-mail-ui", unclaimed=True,
        handle_domain_seed=MAIL_PRIMARY_DOMAIN,
    )
    tmp = tmp_path_factory.mktemp("unclaimed-mail-ui-bridge")
    domain = MAIL_PRIMARY_DOMAIN

    stub_mx = StubMX().start()
    fake_clamd = FakeClamd().start()
    fake_rspamd = FakeRspamd().start()
    op_hatch_extra = (
        f'clamd_addr = "{fake_clamd.addr}"\n'
        f'rspamd_url = "{fake_rspamd.url}"\n'
        f'\n[mta_mx_override]\n'
        f'"external.test" = "{stub_mx.target}"\n'
        f'"{domain}" = "{stub_mx.target}"\n'
    )
    spawned = _spawn_mta_bridge(
        mail_bridge_binary=mail_bridge_binary,
        seal_helper_binary=seal_helper_binary,
        nest_instance=nest,
        tmp=tmp,
        bridge_id="ui-mta-1",
        domain=domain,
        operator_hatch_extra=op_hatch_extra,
        pre_approve=False,
        wait_for_serving=False,
    )
    try:
        yield UnclaimedMailNestUiHandle(
            nest=nest,
            domain=domain,
            claim_code=CLAIM_CODE,
            mx_port=spawned.mx_port,
            metrics_port=spawned.metrics_port,
            stub_mx=stub_mx,
            bridge_ed_pubkey_hex=spawned.ed25519_pubkey.hex(),
            bridge_id=spawned.bridge_id,
            tls_blob=spawned.tls_blob,
            mta_log_file=spawned.log_file_path,
            mta_proc=spawned.proc,
        )
    finally:
        spawned.cleanup()
        stub_mx.stop()
        fake_clamd.stop()
        fake_rspamd.stop()
        nest_cleanup()


@dataclass
class CalDavVariantNestHandle:
    """One built cell of the CalDAV onboarding-variant matrix (plan Task 2): a
    fresh UNCLAIMED nest + an UNAPPROVED MTA + a self-enrolling MDA (with a
    CalDAV listener port), for a given ``address_type``.

    Like ``UnclaimedMailNestUiHandle`` (unclaimed nest + unapproved MTA) but
    ALSO carries an MDA (like ``DedicatedMailNestHandle``), and the nest's
    ``handle_domain`` is parametrized by ``address_type`` so the downstream
    matrix can drive the address-type-specific onboarding (CalDAV's derived
    enablement default is "ON iff the handle is a real domain"). The nest is
    left UNCLAIMED (``nest["admin"] is None``): the
    matrix claims it + approves the bridges through the client UI per cell, so
    neither bridge is serving yet when this handle is returned — ``mda.proc`` is
    alive (still polling for enrollment) but binds no CalDAV listener until the
    UI approves it.

    Attrs:
      ``nest`` — the ``start_nest`` dict (``admin`` is ``None`` — unclaimed).
      ``nest_url`` — ``http://127.0.0.1:{port}``.
      ``domain`` — the handle domain for this address type: ``"fauna.test"``
        (real_domain), ``"localhost"`` (localhost), or ``"127.0.0.1:{port}"``
        (ip — the nest's own loopback authority).
      ``claim_code`` — the one-time claim code (``CLAIM_CODE``) the UI claims with.
      ``caldav_port`` — the MDA's CalDAV-HTTPS listener port (self-signed; the
        MUA connects ``verify=False`` once the bridge is approved + serving).
      ``mx_port`` — the unapproved MTA's port-25 inbound listener port.
      ``mta`` — the spawned MTA handle (``_SpawnedMtaBridge``).
      ``mda`` — the spawned MDA handle (``_SpawnedMdaBridge``; ``.proc.poll()``
        is ``None`` while alive).
    """

    nest: dict
    nest_url: str
    domain: str
    claim_code: str
    caldav_port: int
    mx_port: int
    mta: object
    mda: object


@pytest.fixture
def killable_app(app):
    """The parametrized `app`, gated to drivers that can crash their own app
    child. NOT a structural impossibility (testing.md convention 7 mode 5 —
    driver-capability predicates): `PlatformDriver.kill_uncleanly`'s own
    docstring calls the remaining gap a TODO per driver (web/windows/ios/
    android each need their own shape), so this is temporary debt, declared
    through `skip_unbuilt` like any other unbuilt surface — not a bare skip a
    static scan can't see. (This exact bare-skip shape hid all 11 of
    `test_crash_recovery_journeys.py`'s journeys on tui for as long as the
    driver existed, because `supports_unclean_kill` names no app and no `is_X`
    predicate.)

    In conftest rather than in one test module because a SIGKILL-at-a-seam
    journey is not one file's idea: the provisioning-slot journeys
    (`test_provisioning_slot_recovery.py`) ask the same question of the client's
    own long-term store, and a second copy of this gate is exactly the per-app /
    per-file divergence the declared-skip machinery exists to prevent — two
    copies drift, and the one that stops declaring its class goes invisible to
    `check_app_gate_ratchet.py`.
    """
    from helpers.app_surface import skip_unbuilt

    if not app.driver.supports_unclean_kill():
        skip_unbuilt(
            app.driver,
            surface="the unclean-kill primitive (driver-owned Popen/pty child)",
            detail="only drivers that own their app child as a Popen or pty "
            "session leader implement this today — see "
            "PlatformDriver.kill_uncleanly",
            tracked="testing.md",
        )
    return app


@pytest.fixture
def provision_target_nest(request, nest_mode, tmp_path_factory):
    """A real, never-claimed nest standing in for the box a provisioning run builds.

    Since `onboarding.md` § 6 *Provisioning = build + claim*, a standard-path run
    does not merely build: its `Online` step ends by claiming the box over real
    WS-RPC. A fake cloud serves no WS-RPC, so a run pointed entirely at one can no
    longer reach `Succeeded` — the claim fails and the step lands `Failed`. A test
    that wants a *completed* run therefore needs a real nest for the nest leg, with
    the VPS/DNS legs still faked.

    Wire it as the `nest` entry of `set_provider_base_urls` (`{**fake_cloud.url_map(),
    "nest": nest["url"]}`): that one override carries both the orchestrator's Online
    health poll (`{nest_base_url}/api/v1/health`) and the machine's claim, because
    `WsNestApi::resolve()` prefers it over the caller's URL. The reach address cannot
    be used instead — both dial mechanisms pin port 443 and a harness nest is on a
    random high port.

    `unclaimed=True` leaves the nest with no admin and the harness's `CLAIM_CODE`
    already on disk, so a test pins that code into the run
    (`set_provision_claim_code_for_test`) rather than letting it mint one the box
    has never heard of.

    ⚠ Under this override the first-contact identity root is **inert**: it is keyed
    by the domain while the resolved URL's host is loopback, and the host-match guard
    is `.then_some(…)`, so no root is found and graduation falls back to TOFU. The
    injected-seed trust path stays covered by `pending_provision_slot.rs`'s wiremock
    family and the live provision run, never by a journey using this fixture.

    Stoppable: a test that needs the box to be *absent* at first (so the Online step
    parks in its retry backoff, giving a window to Cancel) calls `stop_nest` on it
    and brings it back with `start_nest_in_place` — the same port and data dir, so
    the claim code survives. Neither of those is a local-binary act: `stop_nest`
    signals `nest["proc"]`, which docker answers with `_ContainerProc`, and
    `start_nest_in_place` delegates to the `start_in_place` key the container
    provider publishes. `test_offline_gate.py` drives that pair against the
    session nest in either mode, which is the standing proof.

    It therefore goes through `_start_dedicated_nest` like every other dedicated
    nest (`testing.md` § Default app and nest mode, ruling (1) — every nest the
    harness starts is the mode provider's to start), rather than calling
    `common.nest.start_nest` behind both entry points, which is a shape neither
    AST pin in `test_nest_mode_axis.py` can grade. Both start options it asks for
    — `unclaimed` and `cors_origins` — every provider that starts a nest honours.

    ⚠ This nest is the one a **browser** dials RAW. Every other web fixture goes
    through the SPA proxy, which is same-origin; the `nest` provider override
    deliberately does not, because the orchestrator's own wasm probe client is no
    part of the login/proxy machinery — it just `fetch`es
    `{nest_base_url}/api/v1/health`. So a fresh nest, whose empty `cors_origins`
    collapses to `DEFAULT_CORS_ORIGIN` (`https://app.fauna.social`), answers that
    fetch with no `Access-Control-Allow-Origin`, the browser blocks the response,
    and the `Online` step retries on reqwest-wasm's generic "error sending
    request" until the test's budget runs out — never on any native app, which is
    outside a browser and sees no CORS at all. `cors_origins` seeds the run's own
    SPA origin the way the deployment artifact seeds the real one, so the poll
    makes a real cross-origin CORS decision instead of an impossible one.
    """
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "provision-target", unclaimed=True,
        cors_origins=browser_origins_to_allow(request),
    )
    yield nest
    # Idempotent on a nest a test already stopped: standalone terminates an
    # exited proc, docker removes a stopped container.
    cleanup()


@pytest.fixture
def unclaimed_caldav_nest(
    mail_bridge_binary, seal_helper_binary, nest_binary, tmp_path_factory
):
    """Factory fixture → ``make(address_type) -> CalDavVariantNestHandle``.

    Each ``make()`` call stands up a fresh UNCLAIMED nest + an UNAPPROVED MTA +
    a self-enrolling MDA, with the nest's ``handle_domain_seed`` chosen by
    ``address_type``:

      - ``"real_domain"`` → ``MAIL_PRIMARY_DOMAIN`` (``"fauna.test"``),
      - ``"localhost"``   → ``"localhost"``,
      - ``"ip"``          → the nest's own ``"127.0.0.1:{port}"`` loopback
        authority (computed AFTER the port is allocated).

    This is essentially ``unclaimed_mail_nest_ui`` (unclaimed nest + unapproved
    MTA) PLUS an MDA spawn (like ``dedicated_mail_nest``), parametrized for the
    handle domain — the matrix (plan Task 4) calls it once per cell, claims the
    nest + approves the bridge(s) through the client UI, then runs the round-trip.

    Both bridges spawn UNAPPROVED (``pre_approve=False``, ``wait_for_serving=
    False``): they self-enroll (``request_enrollment`` → a PENDING row) and bind
    no listeners until the client UI approves them, so nothing here needs the
    admin keypair (the nest is unclaimed). The DNS-perimeter stubs
    (clamd/rspamd/stub-MX) and the spam-policy clear are provisioned by the
    matrix through the UI / accepted scaffolding, not here.

    Multiple ``make()`` calls per test are supported (each gets its own nest +
    bridges); ALL are torn down in REVERSE creation order on fixture teardown,
    mirroring ``unclaimed_mail_nest_ui``/``dedicated_mail_nest`` (process safety:
    owns its nest + bridge subprocesses + the in-process stubs; never
    pkill/killall).
    """
    from drivers.port_util import find_free_port
    from helpers.stub_mx import StubMX
    from fakes.fake_clamd import FakeClamd
    from fakes.fake_rspamd import FakeRspamd

    cleanups = []  # (label, fn) — invoked in REVERSE on teardown

    def make(address_type: str) -> CalDavVariantNestHandle:
        # ── Allocate the nest port up front so the `ip` handle domain (the
        # nest's own loopback authority) can be computed BEFORE start, mirroring
        # `cross_nest_foreign`'s `authority = f"127.0.0.1:{port}"` pattern.
        port = find_free_port()
        # All three arms are `handle_domain_seed` values, and it is the
        # `unclaimed=True` below that decides that — not the shape of the value.
        # The domained claim needs a claim of the HARNESS's to ride on, and this
        # fixture exists precisely so the CLIENT makes that claim through the UI.
        # So the `real_domain` arm is a seed here even though `fauna.test` would
        # be perfectly claimable on a nest the harness claimed itself
        # (`testing.md` § Default app and nest mode, ruling (3)).
        if address_type == "real_domain":
            handle_domain = MAIL_PRIMARY_DOMAIN
        elif address_type == "localhost":
            handle_domain = "localhost"
        elif address_type == "ip":
            handle_domain = f"127.0.0.1:{port}"
        else:
            raise ValueError(
                f"unknown address_type {address_type!r} "
                "(expected real_domain / localhost / ip)"
            )

        tmp_dir = str(tmp_path_factory.mktemp(f"caldav-variant-{address_type}"))
        nest = start_nest(
            nest_binary, tmp_dir, port=port, unclaimed=True,
            handle_domain_seed=handle_domain,
        )

        def _nest_cleanup(_nest=nest):
            _nest["proc"].terminate()
            try:
                _nest["proc"].wait(timeout=10)
            except subprocess.TimeoutExpired:
                _nest["proc"].kill()
                _nest["proc"].wait()

        cleanups.append(("nest", _nest_cleanup))

        # The MDA bridge sets its own CalDAV listener; the MTA + the stub MX /
        # fake scanners give the matrix an outbound + inbound surface to enable
        # through the UI (matching `unclaimed_mail_nest_ui`). The domain the
        # bridges enroll/serve for is the local mail domain (`fauna.test`),
        # which the UI add-domain step claims — independent of the nest's
        # handle domain (the client's outbound From), so a localhost/ip nest
        # still serves CalDAV at the claimed `fauna.test` once approved.
        domain = handle_domain
        # Each bridge gets its OWN tmp subdir. Both _spawn_mta_bridge and
        # _spawn_mda_bridge write `operator-hatch.toml` and use `data_dir = tmp`
        # — co-locating them in one `tmp` makes the second spawn CLOBBER the
        # first's operator-hatch (and share its data_dir). In `dedicated_mail_nest`
        # that's benign (both pre_approve + bind eagerly at spawn, before the
        # clobber). Here both are `pre_approve=False`, so the MTA defers its bind
        # until UI approval — by then the MDA's hatch (which has NO `mta_bind_addr`)
        # has overwritten the MTA's, and the MTA falls back to the privileged `:25`
        # → "bind: permission denied". Separate dirs keep each hatch + data_dir
        # private. (The existing role-specific log-filename split was a partial
        # patch of this same collision; this fixes it at the root.)
        tmp_base = tmp_path_factory.mktemp(f"caldav-variant-{address_type}-bridges")
        tmp_mta = tmp_base / "mta"
        tmp_mta.mkdir()
        tmp_mda = tmp_base / "mda"
        tmp_mda.mkdir()

        stub_mx = StubMX().start()
        cleanups.append(("stub_mx", stub_mx.stop))
        fake_clamd = FakeClamd().start()
        cleanups.append(("fake_clamd", fake_clamd.stop))
        fake_rspamd = FakeRspamd().start()
        cleanups.append(("fake_rspamd", fake_rspamd.stop))
        op_hatch_extra = (
            f'clamd_addr = "{fake_clamd.addr}"\n'
            f'rspamd_url = "{fake_rspamd.url}"\n'
            f'\n[mta_mx_override]\n'
            f'"external.test" = "{stub_mx.target}"\n'
            f'"{domain}" = "{stub_mx.target}"\n'
        )

        # Unapproved MTA (self-enrolls; binds nothing until the UI approves it).
        mta = _spawn_mta_bridge(
            mail_bridge_binary=mail_bridge_binary,
            seal_helper_binary=seal_helper_binary,
            nest_instance=nest,
            tmp=tmp_mta,
            bridge_id=f"caldav-variant-mta-{address_type}",
            domain=domain,
            operator_hatch_extra=op_hatch_extra,
            pre_approve=False,
            wait_for_serving=False,
        )
        cleanups.append(("mta", mta.cleanup))

        # Unapproved MDA (self-enrolls; binds no CalDAV listener until approved).
        mda = _spawn_mda_bridge(
            mail_bridge_binary=mail_bridge_binary,
            nest_instance=nest,
            tmp=tmp_mda,
            bridge_id=f"caldav-variant-mda-{address_type}",
            domain=domain,
            pre_approve=False,
            wait_for_serving=False,
        )
        cleanups.append(("mda", mda.cleanup))

        return CalDavVariantNestHandle(
            nest=nest,
            nest_url=nest["url"],
            domain=domain,
            claim_code=CLAIM_CODE,
            caldav_port=mda.caldav_port,
            mx_port=mta.mx_port,
            mta=mta,
            mda=mda,
        )

    try:
        yield make
    finally:
        for _label, fn in reversed(cleanups):
            try:
                fn()
            except Exception:
                pass


@pytest.fixture(scope="session")
def second_nest(request, nest_mode, tmp_path_factory):
    """Second nest for cross-federation tests."""
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "nest2")
    yield nest
    cleanup()


@pytest.fixture(scope="session")
def third_nest(request, nest_mode, tmp_path_factory):
    """Third nest for multi-node tests."""
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "nest3")
    yield nest
    cleanup()


@pytest.fixture()
def two_nodes(request, nest_mode, tmp_path_factory):
    """Two fresh, independent nests of this run's mode — the HOST-DRIVEN two-nest
    fixture, and the most consumed second-nest shape in the suite (18 files).

    Function-scoped and deliberately so: consumers register their own actors on
    these nests, and every other multi-nest fixture here is session-scoped and
    therefore shared. Every nest is content-ready from first boot (no-modes
    retirement, ratified 2026-07-12), so there is no storage-mode commit before
    post ingest / blob upload / CalDAV work; a test needing some *other* nest
    state starts its own through the same seam rather than adding an option here.

    **Host-driven, not nest-dials-nest.** The harness drives each nest itself;
    neither nest is ever handed the other's authority. That is what makes the
    family collectable in docker at all — a nest-DIALS-nest journey between two
    containers is refused by `validate_peer_url`'s globally-routable requirement
    and is exclusion class (8) (`testing.md` § Default app and nest mode, ruling
    (2)). A test here that starts reading `nest_b["peer_url"]` is asking for that
    other thing, and the class-(8) selector will classify it out of docker on the
    strength of that read alone — which is the intended signal, not a surprise.

    ⚠ **`nest_a`/`nest_b` are the handles; `port_a`/`port_b` are conveniences off
    them, never a second source of truth.** Both are published because the bare
    port is genuinely sufficient for the ~116 helper calls that take one: those
    helpers resolve the scheme centrally through `common.auth.port_base_url`,
    which reads the port→scheme registry `_as_nest_handle` populates, so a
    docker nest's `https://` is already correct at every one of them. What is
    NOT sufficient is composing a URL by hand from the port — see
    `test_nest_mode_axis.py`'s
    `test_no_two_nest_consumer_composes_a_nest_url_by_hand`, which is the pin
    that keeps this fixture's docker collection honest.

    Until 2026-08-30 this lived twice, verbatim, in `tests/api/conftest.py` and
    `tests/platform/conftest.py`, spawning `common.nest.start_nest` directly
    around both nest-start entry points — so it was graded by neither AST pin and
    held the whole two-nest family out of docker (priority #1: one shape, one
    home).
    """
    a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "two-nodes-a")
    try:
        b, cleanup_b = _start_dedicated_nest(
            request, nest_mode, tmp_path_factory, "two-nodes-b")
        try:
            yield {
                "nest_a": a,
                "nest_b": b,
                "port_a": a["port"],
                "port_b": b["port"],
                "admin_token_a": a["admin"]["token"],
                "admin_token_b": b["admin"]["token"],
                "admin_sk_a": a["admin"]["signing_key"],
                "admin_sk_b": b["admin"]["signing_key"],
            }
        finally:
            cleanup_b()
    finally:
        cleanup_a()


@pytest.fixture
def stoppable_nest(request, nest_mode, tmp_path_factory):
    """A function-scoped nest the test may take offline **mid-run**.

    Every other nest fixture is session-scoped and therefore un-stoppable: other
    tests are still using it. A test whose subject is what happens when a nest
    *goes away* — the backup audit's `Overdue` arm is the first — needs its own.

    ``nest["stop"]()`` terminates it and is idempotent; the fixture stops it
    again at teardown if the test did not. Only the handle this fixture itself
    spawned is ever signalled — never a name-based `pkill`, which would hit
    sibling sessions' nests.

    **It is a causal barrier, not a wait** (testing.md convention 14): when
    ``stop()`` returns the port is dead, so an assertion about unreachability
    follows from it directly and needs no settle-sleep.
    """
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "stoppable")
    stopped = {"done": False}

    def stop():
        if not stopped["done"]:
            stopped["done"] = True
            cleanup()

    nest["stop"] = stop
    yield nest
    stop()


@pytest.fixture
def rotatable_nest(request, nest_mode, tmp_path_factory):
    """A function-scoped nest whose **deployment identity** a test may rotate.

    Rotation is deployment-destructive by design (`nest/box-recovery.md`
    § Deployment-seed rotation): from the commit on, the box serves a different
    `nest_actor_id` and signs every channel binding with the successor only, so
    every app pinned to the old identity is on the identity-changed path
    until it walks the rotation chain. Doing that to the session-scoped
    `nest_instance` would change the identity under every later test in the run
    — the same reason `stoppable_nest` exists for tests that kill a nest.

    Function-scoped rather than session-scoped so each test starts from an
    un-rotated box and can assert on `seq` absolutely (a first rotation is
    always seq 1) instead of relative to whatever a previous test left behind.
    """
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "rotatable")
    yield nest
    cleanup()


@pytest.fixture(scope="session")
def second_user(second_nest):
    """User registered on the second nest, with a default feed."""
    return _make_user(second_nest)


@pytest.fixture(scope="session")
def third_user(third_nest):
    """User registered on the third nest, with a default feed."""
    return _make_user(third_nest)


@pytest.fixture(scope="session")
def cross_nest_foreign(request, nest_mode, tmp_path_factory):
    """A nest configured as a *foreign* federation peer for cross-nest
    conversation tests.

    Unlike ``second_nest`` (a bare peer), this one advertises a ``handle_domain``
    equal to its own loopback authority ``127.0.0.1:<port>`` and opens
    self-service registration — so a *handled* actor can be provisioned over the
    wire (``fauna.account.register`` → ``create_user_with_handle``) and
    ``fauna.actor.by_handle`` replies with a domain the originating nest's relay
    can actually reach. This is the e2e-binary mirror of the in-process foreign
    nest F in ``bins/fauna-nest/tests/conformance_cross_nest_conversations_client.rs``
    (which sets ``RegistrationConfig { handle_domain: Some(authority) }``). The
    extra ``authority`` key holds the bare ``host:port`` (the handle suffix).

    ``serve_tls=True``: since Pillar C (``730303718``), ``resolve_handle_domain``
    derives **uniform https** for any non-public authority (loopback included —
    ``fauna-provisioning::probe::resolve_handle_domain``), so the home nest's
    anonymous discovery (``actor_by_handle_remote``) and federation relay both
    dial ``wss://`` here. A plain-HTTP foreign nest fails that handshake and
    silently falls through to the SMTP rail — the e2e twin of the in-process fix. ``nest["url"]`` is therefore ``https://…``.
    """
    from common.auth import open_registration
    from common.nest import OWN_DIAL_AUTHORITY

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-foreign",
        handle_domain_seed=OWN_DIAL_AUTHORITY, serve_tls=True,
    )
    open_registration(nest)
    nest["authority"] = nest["handle_domain_seed"]
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="function")
def cross_nest_foreign_actor(cross_nest_foreign):
    """Bob: a *handled* actor on the foreign nest with two published real MLS key
    packages, so ``fauna.actor.by_handle`` reports him ``addressable``. Returns
    his actor dict (``signing_key`` / ``actor_id_hex`` / ``actor_id_bytes`` /
    ``token`` / ``handle``) plus the foreign ``authority``.

    Two key packages so a single cross-nest 1:1 bootstrap (which consumes one)
    leaves the precondition assertion something to observe before and after.

    **FUNCTION-scoped, with a per-test handle** (was session-scoped through
    2026-08-29). A consuming test asserts bob's key-package count *exactly*
    (2 before the bootstrap, 1 after), so a bob shared across tests makes those
    counts a function of collection order: the app parametrization that runs
    first consumes a package and every later one starts from a different number.
    That was latent for as long as the web arm silently never consumed anything;
    the moment it started working, ``--app web,linux`` (and therefore
    ``--app sweep``) failed on whichever app ran second. A fresh actor per test
    removes the coupling outright rather than teaching each assertion to guess
    how many peers ran before it. The handle carries a random suffix because the
    foreign nest itself is session-scoped, so a fixed ``bob`` would collide with
    the previous test's registration on it.
    """
    return provision_foreign_handled_actor(cross_nest_foreign)


def provision_foreign_handled_actor(foreign: dict, key_packages: int = 2) -> dict:
    """Register a fresh handled actor (``bob<random>``) on the foreign nest
    ``foreign`` (a ``cross_nest_foreign``-shaped dict) and publish
    ``key_packages`` real MLS key packages for it, so ``fauna.actor.by_handle``
    reports it ``addressable``. Returns the actor dict plus ``authority``.
    Shared by the session-scoped ``cross_nest_foreign_actor`` and the
    per-test ``cross_nest_foreign_ephemeral`` fixtures.
    """
    import secrets
    from common.auth import register_handled_actor
    from tests.api import conv_api
    bob = register_handled_actor(
        foreign["port"], handle=f"bob{secrets.token_hex(4)}",
        domain=foreign["authority"], base_url=foreign["url"],
    )
    conv_api.keypackage_upload(
        foreign["port"], bob,
        conv_api.mint_key_packages(bytes(bob["signing_key"]), key_packages),
        scheme="https",
    )
    bob["authority"] = foreign["authority"]
    return bob


@pytest.fixture(scope="function")
def cross_nest_foreign_ephemeral(request, nest_mode, tmp_path_factory):
    """A **per-test, stoppable** foreign federation peer — the same shape as
    ``cross_nest_foreign`` (own loopback authority as ``handle_domain``, open
    registration, floor TLS) with one handled, addressable actor already
    provisioned under ``nest["actor"]``, plus ``nest["stop"]()`` which
    terminates the nest *mid-test*.

    Exists for the discovery-failure tests
    (``docs/goal/architecture/federation.md`` § Peer-auth model → *Discovery-failure
    semantics*): they need a peer that is reachable for a first contact and then
    **goes away**, which the session-scoped ``cross_nest_foreign`` — shared by
    every other cross-nest test — must never do. ``stop`` is the fixture's own
    teardown applied early (the process is this fixture's child; the
    process-safety rule forbids killing *other* sessions' nests, not one's
    own), and teardown is idempotent after it.
    """
    from common.auth import open_registration
    from common.nest import OWN_DIAL_AUTHORITY

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-foreign-ephemeral",
        handle_domain_seed=OWN_DIAL_AUTHORITY, serve_tls=True,
    )
    open_registration(nest)
    nest["authority"] = nest["handle_domain_seed"]
    nest["actor"] = provision_foreign_handled_actor(nest)

    # `stop` IS the fixture's own teardown, applied early — so it is the
    # provider's `cleanup`, not a hand-rolled terminate/wait. Idempotence is the
    # contract (teardown calls it again after a test already has), and it lives
    # in this flag rather than in a `proc.poll()` check: the provider owns what
    # "stopped" means for its own nest, and a caller peering at a process handle
    # is exactly the harness-side stand-in for a nest fact that kept these
    # fixtures out of the mode provider in the first place.
    stopped = False

    def stop() -> None:
        nonlocal stopped
        if stopped:
            return
        stopped = True
        cleanup()

    nest["stop"] = stop
    try:
        yield nest
    finally:
        stop()


@pytest.fixture(scope="function")
def caldav_cross_nest_peer(request, nest_mode, tmp_path_factory):
    """A function-scoped FOREIGN, email-disabled peer nest (nest B) for the
    cross-nest CalDAV server-side auto-schedule test.

    Like ``cross_nest_foreign`` it advertises ``handle_domain`` = its own loopback
    authority ``127.0.0.1:<port>`` (so the organizer-nest MDA's anonymous discovery
    — ``resolve_handle_domain`` maps the loopback authority straight to
    ``https://127.0.0.1:<port>`` (uniform-https since Pillar C, ``730303718``;
    ``serve_tls=True`` below is what makes that resolve actually connect) — AND
    the organizer nest's federation relay can actually reach it) and opens
    self-service registration so a test can seed a handled, mailbox-less Fauna
    attendee on it. It **never** enables mail, so
    ``fauna.setup.status.email_enabled`` reports ``false``
    (``discovery_core.rs`` ``get_mail_enabled().unwrap_or(false)``) — which is what
    makes a handled actor on it a CROSS-NEST *mailbox-less* attendee the organizer
    nest's MDA routes onto the sealed scheduling rail (``caldav-server.md``
    § Server-side auto-schedule; ``resolve_attendee_transport`` keys on the peer's
    ``email_enabled``). Function-scoped (vs the session-scoped ``cross_nest_foreign``)
    so each cross-nest test gets a fresh, isolated peer — the organizer nest A
    (``dedicated_caldav_mailbox_less_nest``) it pairs with is function-scoped too.

    Process safety: owns its nest subprocess; tears it down on teardown, never
    pkill/killall.
    """
    from common.auth import open_registration
    from common.nest import OWN_DIAL_AUTHORITY

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-caldav-xnest-peer",
        handle_domain_seed=OWN_DIAL_AUTHORITY, serve_tls=True,
    )
    open_registration(nest)
    nest["authority"] = nest["handle_domain_seed"]
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def real_faunamls_app(logged_in_app):
    """alice opted into the REAL ``FaunaMlsBackend`` (over her client's
    conversations RPC seam — ``NestConversationsRpc`` native / ``WsConversationsRpc``
    wasm), with the mock restored on teardown so a later snapshot conversations
    test (collection order can put one after this test, sharing the
    session-cached app) keeps the mock.

    Two activation shapes, chosen per client (**generalized 2026-07-15** — Track E's
    launch-gate half landed on windows + apple; android followed 2026-07-20):

    * **linux / web / tui** — a RUNTIME toggle. ``enable_real_faunamls()`` flips the
      session-cached app's mock backend live (waiting for
      ``data.conv_real_backend_active``) and ``disable_real_faunamls()`` undoes it in
      teardown.
    * **windows** — a LAUNCH-TIME gate (``FAUNA_E2E_REAL_CONVERSATIONS``, set by
      ``_apply_real_conversations_env`` when the CONSUMING test carries
      ``@pytest.mark.real_conversations``): the real manager registers unconditionally
      at process launch. ``conversations_enable_real_faunamls`` is an intentional
      no-op query there (``ConversationsCommands.EnableRealFaunaMls``) — still called
      here as a readiness POLL against ``data.conv_real_backend_active`` — but there is
      no disable command, so teardown skips it.
    * **macOS / iOS / android** — the same launch-time gate, but with NEITHER an
      enable NOR a disable command (confirmed: zero hits for
      ``conversations_enable_real_faunamls``/``conv_real_backend_active`` anywhere in
      ``apps/fauna-apple/`` or ``apps/fauna-android/``). This fixture makes no bridge
      call at all for them, since neither has a client-side "is it real yet" signal
      today. **The control that closes this is the app's OWN LOG, and since
      2026-09-22 this fixture runs it for them** —
      ``helpers/real_rail_control.witness_real_rail`` polls for the apple shells'
      gate verdict and then for ``mls-sync:``, which only
      ``ConversationsSession::start_receive_loop`` can put there and so only the
      real rail reaches. That module owns the whole rationale, including why the
      consuming test's own markers cannot answer the question and which of the
      launcher's arms are observable. A driver with no app-log reader takes its
      ``skip_unbuilt`` there, and web its ``declared_absence`` (native-only marker).
      ⚠ **Not ``keypackage_count`` going non-zero**, which this docstring used to
      recommend and which is *unsound* here: ``test_user`` is session-scoped, so in a
      multi-app invocation an earlier app may have published alice's packages and the
      poll passes without this app ever leaving the mock. A control another app can
      satisfy on your behalf is not a control. android joined 2026-07-20
      (``FAUNA_E2E_REAL_CONVERSATIONS`` reaches the app as an
      intent extra — ``BridgeHttpServer`` → ``AppLauncher`` →
      ``TestAgent.isRealConversationsActive`` → ``ConversationsManagerHost``), and
      its ``conversations_real_*`` agent arms landed 2026-07-30.

    ⚠ **A windows/macOS/iOS test consuming this fixture WITHOUT the
    ``real_conversations`` marker now raises a legible error on ALL THREE** — windows
    because its readiness poll times out, macOS/iOS because the log control above
    finds the gate ``CLOSED``. Until 2026-09-22 the apple half instead ran silently
    against the MOCK backend and could pass while proving nothing, which is the one
    divergence that made this hole apple-shaped; the three families now fail the same
    way (priorities #1/#3). The fix for such a test is a marker, not a bypass: mark
    the consuming test AND run its module in its own pytest invocation
    (``_apply_real_conversations_env`` is session-wide per collected session, not
    per-test — mixing it into an invocation with mock-inject DM tests would flip those
    to real too). A test whose app scope genuinely excludes these families declares
    that scope with an APP MARKER instead, so collection deselects it before any
    fixture runs — the shape ``pytest_collection_modifyitems`` already prefers over an
    in-body skip, and the only shape that reaches a guard the fixture would otherwise
    pre-empt.

    Shared by the same-nest (``test_fauna_mls_real_roundtrip``, also
    ``--client windows`` — its ``conversations_real_{add,remove,rename}`` commands
    landed) and cross-nest (``test_fauna_mls_cross_nest_roundtrip``, still
    linux/web/tui-only) real-wire tests: they drive
    ``real_add``/``real_remove``/``real_rename``, which macOS/iOS still lack — the
    narrower remaining Track E. android's landed 2026-07-30 (``TestAgent.kt``
    ``dispatchConversationsManagerCommand``), so android is no longer the blocked
    leg here; what it still lacks is a machine to run on (the host emulator —
    every android e2e track carries that same gate)."""
    yield from _activate_real_faunamls(
        logged_in_app, context="the `real_faunamls_app` fixture"
    )


def _activate_real_faunamls(app, *, context):
    """Shared body behind ``real_faunamls_app`` and
    ``domainless_real_faunamls_app``: put an already-logged-in ``app`` on the
    REAL ``FaunaMlsBackend`` and yield it, restoring the mock on teardown where
    the app has a runtime toggle. ``real_faunamls_app``'s docstring owns the
    per-client activation shapes; ``context`` names the caller in the
    real-rail witness's failure, so a red names the fixture whose promise went
    unmet."""
    driver = app.driver
    runtime_toggle = driver.is_linux() or driver.is_web() or driver.is_tui()
    launch_gate_with_query = driver.is_windows()
    launch_gate_silent = driver.is_macos() or driver.is_ios() or driver.is_android()
    if not (runtime_toggle or launch_gate_with_query or launch_gate_silent):
        pytest.skip(
            "real-wire FaunaMls backend is linux + web + tui (runtime toggle) or "
            "windows + macOS + iOS + android (launch-time real_conversations gate) "
            "today"
        )
    if runtime_toggle or launch_gate_with_query:
        # enable_real_faunamls waits for AuthSuccess (which initializes alice's MLS
        # engine + stashes the backend deps) so the engine is ready before any send.
        # On windows this is a no-op query (see docstring) but still a real
        # readiness poll against data.conv_real_backend_active.
        app.conversations.enable_real_faunamls()
    elif launch_gate_silent:
        # No readiness QUERY exists for these apps, so the fixture's promise —
        # "an app on the REAL FaunaMls backend" — used to be unwitnessed here and
        # a consumer could pass against the mock. The app's own log is the one
        # witness available (`helpers/real_rail_control.py` owns why, and why the
        # consuming test's markers cannot answer it). Deliberately the SAME shape
        # as the windows branch above: an unmet promise is a legible fixture
        # error, not a silent yield (priorities #1/#3 — windows has raised here
        # since the readiness poll landed, and apple diverging from that is the
        # only reason the hole was apple-shaped).
        from helpers import real_rail_control

        real_rail_control.witness_real_rail(
            driver, context=context
        )
    try:
        yield app
    finally:
        if runtime_toggle:
            app.conversations.disable_real_faunamls()


def _launch_second_real_faunamls_app(
    request, real_faunamls_app, nest_instance, test_user, *, suppress_push, fixture_name
):
    """Shared body behind ``second_real_faunamls_app`` /
    ``second_real_faunamls_app_push_only`` — everything except the one env var
    that decides which of bob's two receive arms is live. See those fixtures'
    own docstrings for the property each combination proves; this helper only
    carries what does not vary: bob's registration/reachability, his ticker
    mute, his login, and his real-engine activation.

    ``fixture_name`` is the caller's own name, and it exists for ONE reason: it
    goes into the real-rail witness's ``context`` below, so a red names the
    fixture whose promise went unmet rather than "the second app". Derived from
    ``suppress_push`` it would be clever and wrong the day a third combination
    lands.
    """
    alice = real_faunamls_app
    if alice.driver.is_linux():
        app_name = "linux"
    elif alice.driver.is_tui():
        app_name = "tui"
    elif alice.driver.is_macos():
        # row 283: apple's per-launch isolation (e2e-conventions.md point 10 —
        # fresh XDG dirs, per-launch keyring/agent port, private D-Bus) already
        # gives a second, independently-authenticated app process/session for
        # free on macOS — no per-fixture change needed beyond naming it here.
        app_name = "macos"
    elif alice.driver.is_ios():
        app_name = "ios"
    elif alice.driver.is_windows():
        # row 168: windows' per-launch isolation (FAUNA_E2E_CREDENTIAL_DIR +
        # FAUNA_E2E_DATA_DIR, `drivers/windows.py`) already gives a second,
        # independently-authenticated app process/session for free — the
        # SAME property `folder_share_owner_app`'s docstring documents
        # windows joining 2026-08-10 for. Takes the apple-shaped
        # launch-gated branch below (real_conversations, not a runtime
        # toggle), like macos/ios.
        app_name = "windows"
    else:
        pytest.skip(
            "two-driven-client native receive proofs run on the direct-Rust "
            "clients (linux + tui), apple (macos + ios), and windows; web's "
            "poll-only leg is tracked with the web client's own backlog"
        )

    # bob: a distinct registered actor on the SAME nest, who ACCEPTS alice.
    #
    # The accept is not politeness — it is what makes bob reachable at all.
    # Since 2026-08-02 the DM plane consults the recipient's
    # inbox mode, and a freshly registered actor's stored mode is the
    # `allow_knock` default, so `dm_initiation_mode_verdict(None, AllowKnock)`
    # = Knock and the nest refuses alice's Welcome with the opaque
    # `fauna.conversations.forbidden` (direct-messages.md § Reach policy).
    # Arranged here rather than in each consuming test because bob is THIS
    # fixture's creation: the fixture that registers a peer is the one that
    # owes his reachability. Without it every consumer failed identically and
    # unhelpfully — the refusal renders as the generic "Something went wrong
    # talking to the nest", so it presented as a missing Welcome two
    # assertions later.
    admin_sk = nest_instance["admin"]["signing_key"]
    bob = create_actor_and_register(
        nest_instance["port"], admin_signing_key=admin_sk
    )
    from tests.api import conv_api

    conv_api.accept_contact(nest_instance["port"], bob, test_user["actor_id_hex"])

    # Reuse the standard launch config for alice's client (app_path) and add
    # the ticker mute, plus the push-suppression knob when this caller wants
    # the drain-alone shape.
    config = _build_app_config(app_name, nest_instance, request)
    if app_name == "ios":
        # bob is a SECOND, concurrently-live iOS app in this session — he
        # cannot share alice's `_get_ios_setup` device (launch()'s
        # own uninstall+install would tear alice's app down mid-test).
        second_udid = _get_ios_second_seat_udid()
        if second_udid is None:
            pytest.skip("could not allocate a second iOS simulator for bob's seat")
        config["udid"] = second_udid
    config.setdefault("environment", {})
    if suppress_push:
        config["environment"]["FAUNA_E2E_SUPPRESS_CONV_PUSH"] = "1"
    # ⚠ **bob's backstop ticker is MUTED (one hour), always** — he is the one
    # app in the suite whose delivery is driven by `conv_receive_now`
    # (`fauna_e2e_agent::CONV_RECEIVE_NOW`) or the push arm instead of a
    # cadence, and that is what makes his consumer able to fail. With the
    # shared 2 s fast-drain tick a test passes whether or not the arm under
    # test does anything: the ticker delivers inside any generous budget, so a
    # broken arm reads GREEN — the "unable to fail" shape convention 14's own
    # build-out keeps finding.
    #
    # This does not weaken what either combination proves. The poke (drain
    # variant) is an arm of the receive loop's own `select!` expanding the
    # identical `full_sweep!` the ticker expands, so the drain → ingest →
    # decrypt chain is byte-for-byte the ticked one; that the ticker exists
    # and is sized from this very variable is pinned at tier_1
    # (`fauna-conversations` `cadence_tests`) — the split convention 14 asks
    # for: cadence logic at tier_1, one wiring proof in the e2e.
    config["environment"]["FAUNA_CONV_POLL_SECS"] = "3600"

    driver = create_driver(app_name)
    driver.launch(config)
    try:
        # Log bob in (the bridge set_state session block `logged_in_app` uses);
        # AuthSuccess builds his conversations session — drain + receive loop +
        # login-time key-package publish — under the arm combination above.
        secret_hex = bob["signing_key"].encode().hex()
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": secret_hex,
                "handle": "bob-e2e",
                "actor_id": bob["actor_id_hex"],
                "device_id": "test-device-bob",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        bob_app = ActionLayer(driver)
        # Activate bob's real engine (replaces the e2e mock) so the delivered
        # Welcome is decrypted by the real MLS engine, and wait until the
        # backend is live.
        bob_app.conversations.enable_real_faunamls()
        if driver.is_macos() or driver.is_ios():
            # BOB's OWN witness, and it is not redundant with alice's.
            #
            # `real_faunamls_app` witnesses ALICE (its launch-gate-silent branch
            # above): that the session-wide env reached HER process. Bob is a
            # separate launch with a separate `ConversationsSession`, built by
            # the `set_state` login two statements up and able to fail on its
            # own — and on these apps the `enable_real_faunamls()` call above
            # returns immediately without asking him anything (`actions/
            # conversations.py::enable_real_faunamls`'s launch-gate branch: no
            # `conversations_enable_real_faunamls` command exists and there is no
            # `data.conv_real_backend_active` to poll). So the second app used to
            # yield UNWITNESSED — and every receive-side assertion in these
            # fixtures' consumers is about bob, which makes his the process the
            # tests are actually about.
            #
            # The `context` names him because convention 6's own rider —
            # diagnose against a witness you HOLD — cuts both ways: a red from a
            # control that cannot say WHICH of two live apps stayed on the mock
            # sends the reader to the wrong process. windows needs no arm here:
            # its `enable_real_faunamls()` IS a real readiness poll against
            # `data.conv_real_backend_active`, so bob's own activation already
            # raises there, and linux/tui wire the real session at every login.
            from helpers import real_rail_control

            real_rail_control.witness_real_rail(
                driver,
                context=f"the `{fixture_name}` fixture (BOB, the second app)",
            )
        yield bob_app, bob
    finally:
        try:
            driver.screenshot("teardown-second-bob")
        except Exception:
            pass
        driver.teardown()


@pytest.fixture
def second_real_faunamls_app(request, real_faunamls_app, nest_instance, test_user):
    """A SECOND real-engine native GUI app (bob), **the same client as alice**
    (``real_faunamls_app``), on the same nest — launched with his conversations
    **push arm suppressed** (``FAUNA_E2E_SUPPRESS_CONV_PUSH=1`` →
    ``conv_push_source`` returns ``None``) and his backstop ticker muted, so bob
    can ONLY receive a Welcome via an explicit ``conv_receive_now`` poke — the
    durable inbox-apply drain proof (``api-layers.md`` § Inbox & Messaging —
    "Durable inbox-apply consumer", layer 5). See ``second_real_faunamls_app_push_only``
    for the opposite combination (the push-arm-alone proof).

    Native-only because the *knob* is native-only, NOT because web lacks a push
    arm — the SPA has had one since 2026-07-12 (``transport.md`` § Push events).
    ``conv_push_source`` is `cfg(not(wasm32))`, so web's mirror lever is the
    opposite one: ``WebBridgeDriver.set_conv_poll_secs`` mutes its *ticker*
    instead, which is what ``test_conv_rail_push_wakes_web`` uses to prove the
    web push arm alone. The supported set is owned by
    ``_launch_second_real_faunamls_app`` and is wider than this sentence used to
    claim: the two direct-Rust clients (linux + tui, which wire the in-process
    ``ConversationsSession`` at every login), **apple** (macos + ios — their
    per-launch isolation supplies the second seat, and bob's own real-rail
    witness runs there) and **windows**; anything else skips in that helper.
    Builds a fresh driver **directly** (NOT
    via the session ``_driver_cache``, which is single-app), logs bob in as a
    *distinct* registered identity via ``set_state`` (mirroring
    ``logged_in_app``), then activates his real backend.

    bob's engine publishes his key packages at login
    (``ConversationsSession::start_receive_loop replenish``) and **keeps the private half**, so —
    unlike the throwaway-engine API-tier peers in ``test_fauna_mls_real_roundtrip``
    — he can DECRYPT the Welcome + the pre-join history.

    Yields ``(bob_app, bob_actor)`` where ``bob_actor`` is his
    ``create_actor_and_register`` dict (signing key / actor id / token), so the
    test can assert his nest-side key-package effects.
    """
    yield from _launch_second_real_faunamls_app(
        request, real_faunamls_app, nest_instance, test_user,
        suppress_push=True, fixture_name="second_real_faunamls_app",
    )


@pytest.fixture
def second_real_faunamls_app_push_only(
    request, real_faunamls_app, nest_instance, test_user
):
    """The opposite combination from ``second_real_faunamls_app``: bob's
    backstop ticker is muted the same way, but his **push arm stays live**
    (``FAUNA_E2E_SUPPRESS_CONV_PUSH`` unset) and nothing pokes his receive
    loop. With the ticker out of the picture, delivery has exactly one
    remaining trigger — the push arm — mirroring the web push-alone proof
    (``test_conv_rail_push_wakes_web``, which mutes the ticker via
    ``WebBridgeDriver.set_conv_poll_secs`` instead of an env var, since web has
    no ``FAUNA_E2E_SUPPRESS_CONV_PUSH`` knob to flip). This is the native leg
    ``test_conv_rail_push_wakes_web``'s own docstring names as untested
    ("the NATIVE conv rail's push arm has no
    alone-proof").

    Yields ``(bob_app, bob_actor)``, same shape as ``second_real_faunamls_app``.
    """
    yield from _launch_second_real_faunamls_app(
        request, real_faunamls_app, nest_instance, test_user,
        suppress_push=False, fixture_name="second_real_faunamls_app_push_only",
    )


@pytest.fixture(scope="module")
def domainless_nest(request, nest_mode, tmp_path_factory):
    """A claimed nest with NO identity domain — a genuinely non-public
    deployment. Its users are per test (``domainless_user``), never on the
    nest: it outlives every app param of the module, and an owner shared
    across them carries the previous param's custody (receipt and all) into
    the next.

    The shared ``nest_instance`` cannot be one: the autouse
    ``_session_primary_mail_domain`` registers ``fauna.test`` as its primary
    mail domain, and a primary domain that reads as a public DNS name becomes
    the deployment identity, so the shared nest reports
    ``is_public_deployment()``. That shared rig is internally inconsistent in
    one respect — public by identity, yet served over plaintext loopback — and
    every nest-side rule keyed on publicness sees the public half. The
    counterparty dial policy is one: on a public nest it refuses a plaintext
    loopback URL outright (``fauna_core::counterparty_url`` — "the loopback
    it would knock on is its own"). A journey whose nest must DIAL a URL this
    rig hands it (the custodian-nest pump dialing the owner's nest) therefore
    runs here, where plaintext loopback is the honest private-nest shape.
    ``atproto_localhost_nest`` is the same carve-out for the Bluesky gate."""
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "domainless-nest", claim_domain=None,
    )
    yield nest
    cleanup()


@pytest.fixture
def domainless_user(domainless_nest):
    """A fresh registered user on ``domainless_nest`` — alice for ONE test.

    Function-scoped on purpose: an account's owner-side state (a minted
    custody, its latest receipt) outlives the test that made it, so a
    module-shared owner would hand the next app param an already-confirmed
    custody row ahead of its own (``test_custody_ceremony_nest_anchored``'s
    no-confirmation-yet state read the previous param's row)."""
    return _make_user(domainless_nest)


@pytest.fixture
def domainless_spa_url(static_dir, domainless_nest):
    """Function-scoped SPA proxy → ``domainless_nest`` (``_login_app_as``: a
    dedicated nest must pass its own proxy for web)."""
    url, server = _serve_spa_proxy(static_dir, domainless_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def domainless_real_faunamls_app(request, app, domainless_nest, domainless_user):
    """``real_faunamls_app``'s twin on ``domainless_nest``: alice (this test's
    ``domainless_user``) logged in there and put on the REAL FaunaMls
    backend. The switch is verified live — a dedicated actor is exactly where
    a silently declined switch reads as an empty page."""
    _login_app_as(app, request, domainless_nest, domainless_user,
                  spa_url_fixture="domainless_spa_url", verify_live_actor=True)
    yield from _activate_real_faunamls(
        app, context="the `domainless_real_faunamls_app` fixture"
    )


@pytest.fixture
def domainless_second_real_faunamls_app(
    request, domainless_real_faunamls_app, domainless_nest, domainless_user
):
    """``second_real_faunamls_app``'s twin on ``domainless_nest`` — bob, same
    client as alice, push arm suppressed. Yields ``(bob_app, bob_actor)``."""
    yield from _launch_second_real_faunamls_app(
        request, domainless_real_faunamls_app, domainless_nest,
        domainless_user, suppress_push=True,
        fixture_name="domainless_second_real_faunamls_app",
    )


@pytest.fixture
def succession_member_app(request, succeedable_app, nest_instance):
    """bob — a second real-engine native GUI app who is a genuine **member**
    of a group owned by a **succeedable** alice: the member-side half of an
    identity succession (``identity-succession.md`` § Propagation → *MLS
    groups*; the statement's whole audience is members like him).

    Yields ``(bob_app, bob_actor)``, alongside ``succeedable_app``'s
    ``(alice_app, alice_user)`` which a consuming test takes directly.

    **Why this is not ``second_real_faunamls_app`` with a different alice.**
    That fixture chains ``real_faunamls_app`` → ``logged_in_app``, so its alice
    IS the session-shared ``test_user`` — and the ceremony this fixture exists
    for **retires alice's identity**, revoking every bearer of it. Run as
    ``test_user`` it would break every later test in the session, which is the
    order-dependence ``ungranted_app``/``succeedable_app`` were minted to kill.
    So alice comes from ``succeedable_app`` (a dedicated actor whose seed the
    test can also name to a nest-side ceremony) and bob is built here over her.

    **Deliberately WITHOUT ``FAUNA_E2E_SUPPRESS_CONV_PUSH``**, unlike
    ``second_real_faunamls_app``: that knob exists to force the durable
    inbox-apply drain backstop for the layer-5 proof. Here the subject is what
    an ordinary member's ordinary receive path renders, so bob keeps every arm
    a real member has.

    bob's engine publishes his key packages at login and keeps the private
    half, so he genuinely decrypts alice's Welcome, her messages, and the
    in-group succession statement. His contact-accept of alice is arranged here
    for the same reason ``second_real_faunamls_app`` arranges its own: since
    2026-08-02 the DM plane consults the recipient's inbox mode, and a
    freshly registered actor's default refuses alice's Welcome with the opaque
    ``fauna.conversations.forbidden`` (``direct-messages.md`` § Reach policy).

    Runs on linux + tui (the two direct-Rust clients that wire the in-process
    ``ConversationsSession``) and on **web**, whose member seat is a twin PAGE
    in its own ``BrowserContext`` (``alice_second_web_device``'s idiom) over
    the wasm ``WebSuccessionChainSource`` dial; every other ``app`` param is
    declared unbuilt below until its own run is green.
    """
    from helpers.app_surface import skip_unbuilt

    alice_app, alice_user = succeedable_app
    if alice_app.driver.is_linux():
        app_name = "linux"
    elif alice_app.driver.is_tui():
        app_name = "tui"
    elif alice_app.driver.is_web():
        # bob's seat is a twin PAGE in its own BrowserContext — isolated
        # localStorage is the per-seat isolation a native arm gets from a
        # second process, and a browser is far too heavy to launch twice
        # (`alice_second_web_device`, `caldav_mailbox_less_attendee_app`).
        # Widened 2026-10-08 on its own green run.
        app_name = "web"
    else:
        # Convention 7: unbuilt debt, declared as such rather than skipped
        # silently — a member seat here could not re-point whatever the harness
        # did, or could not be shown to. Not a platform absence.
        #
        # ⚠ Deliberately NOT widened past linux+tui on 2026-09-03, when the four
        # FFI apps gained the witness and the sweep in Rust: what this fixture
        # needs is a second *real-engine driver* seat, and that half is unproven
        # on every app but these two — mechanism present is not the same claim as
        # member seat demonstrated, and narrowing a ratchet on the first is how a
        # skip turns into a false green. Widen an app's arm when its own run is
        # green, not when its Rust lands.
        skip_unbuilt(
            alice_app.driver,
            surface="a conversations SuccessionWitness member seat (the "
                    "member-side re-point of a succeeded participant, driven "
                    "through a second real-engine app)",
            detail="the witness itself is no longer the gap on the native apps: "
                   "linux and the four FFI apps (macOS, iOS, windows, android) "
                   "all register one as of 2026-09-03, off the shared "
                   "ChainWitness + NativeSuccessionChainSource + peer-anchor "
                   "sweep in fauna-client-recovery. What is unbuilt here is a "
                   "member seat this fixture can drive — only linux, tui "
                   "and web have one",
            tracked="succession-aftermath.md § Implementation status today — "
                    "the ✅ witness bullet",
        )

    admin_sk = nest_instance["admin"]["signing_key"]
    bob = create_actor_and_register(
        nest_instance["port"], admin_signing_key=admin_sk
    )
    from tests.api import conv_api

    conv_api.accept_contact(
        nest_instance["port"], bob, alice_user["actor_id_hex"]
    )

    if app_name == "web":
        # The twin page owns no process: no launch config, and nothing to tear
        # down but the page itself (handled below). The browser reaches the
        # nest through the SPA proxy, as `_login_app_as` logs alice in.
        driver = alice_app.driver.open_twin_page()
        node_url = request.getfixturevalue("spa_url")
    else:
        config = _build_app_config(app_name, nest_instance, request)
        driver = create_driver(app_name)
        driver.launch(config)
        node_url = nest_instance["url"]
    try:
        secret_hex = bob["signing_key"].encode().hex()
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "bob-succession",
                "actor_id": bob["actor_id_hex"],
                "device_id": "test-device-bob-succession",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        bob_app = ActionLayer(driver)
        bob_app.conversations.enable_real_faunamls()
        yield bob_app, bob
    finally:
        try:
            driver.screenshot("teardown-succession-member-bob")
        except Exception:
            pass
        driver.teardown()


def _alice_extra_seat(app_name, request, test_user, nest_instance, *, screenshot_tag):
    """Launch one MORE seat of alice — the same identity as ``logged_in_app``, a
    distinct device id, a fresh empty state root — on ``app_name`` and yield
    ``(seat_app, driver)``. The body of ``alice_second_device`` (whose docstring
    owns the per-app isolation mechanics), shared with ``alice_builder_seats``,
    whose extra seats run on a DIFFERENT app than the one under test."""
    # alice's own secret + actor id (the `logged_in_app` login uses the same
    # `test_user["signing_key"].encode().hex()`); the extra seat logs in with them
    # so it derives the SAME BackupKey that unseals alice's replica.
    secret_hex = test_user["signing_key"].encode().hex()

    # Reuse the standard launch config (fresh state root + the shared
    # FAUNA_CONV_POLL_SECS=2 fast-drain cadence). A brand-new driver launch ⇒ a
    # brand-new empty state dir ⇒ device B truly has nothing until it restores.
    config = _build_app_config(app_name, nest_instance, request)
    if app_name == "macos":
        # Isolate the Application Support store (the MLS store) — see docstring.
        # `dir="/tmp"`, NOT the default `$TMPDIR`: macOS hands pytest a
        # `/var/folders/<..>/T/` tempdir whose length alone pushes the HOME-derived
        # `Library/Application Support/Fauna/sync-agent.sock` past the 104-byte
        # `sun_path` budget (`drivers/macos.py` § the launch-time guard that
        # measures it; same class as the 2026-07-24 multiseat incident, and the
        # same `dir="/tmp"` fix `test_sync_agent_ipc_unix_transport.py::short_tmp_dir`
        # already carries). The agent would otherwise die at bind and the app would
        # report only "agent unreachable".
        config["home"] = tempfile.mkdtemp(prefix="fauna-e2e-alice-device-b-", dir="/tmp")
    elif app_name == "windows":
        # Pin device B's data dir (incl. the native `mls.db` MLS store) — the
        # app-side FAUNA_E2E_DATA_DIR seam the driver honors (`drivers/windows.py`;
        # the version-skew at-rest leg's mechanism, `helpers/skew_client.py`
        # § FAUNA_E2E_DATA_DIR). Device A has its own isolated dir (the driver
        # default since e2e rule 10 landed for windows); device B gets a fresh one
        # THIS fixture owns, so it provably boots with an empty `mls.db` and MUST
        # restore the replica on login — not a same-device restart against a
        # surviving store (see docstring). Explicit, not inherited: the two-device
        # claim must not rest on a driver default a future change could alter.
        config["data_dir"] = tempfile.mkdtemp(prefix="fauna-e2e-alice-device-b-")

    driver = create_driver(app_name)
    driver.launch(config)
    try:
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": secret_hex,
                # The same actor's REAL nest handle as device A (see
                # `_login_app_as`) — a second device of one identity must not
                # report a different one.
                "handle": test_user.get("handle") or "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                # A DISTINCT device id from device A's `_E2E_LOGIN_DEVICE_ID`: same
                # identity, different device. The replica is per-(actor, path),
                # keyed by the shared BackupKey — the device id never gates it.
                "device_id": "fedcba9876543210" * 4,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        device_b = ActionLayer(driver)
        if app_name in ("linux", "tui"):
            # The real backend is wired unconditionally at login now (linux:
            # `conv_backend::request_e2e_activation` just waits on readiness;
            # tui: `start_conversations_session` sets `real_session` at login
            # and this is a readiness poll on `data.conv_real_backend_active`);
            # this confirms device B's session is live before the test drives
            # it. The async `MlsSyncLauncher` restore still runs in the login
            # task, so the test polls the snapshot for the restored thread
            # after this returns.
            device_b.conversations.enable_real_faunamls()
        elif app_name == "windows":
            # windows shares macOS's launch-time real_conversations gate (the real
            # ConversationsSession registers at process launch), but unlike macOS it
            # ALSO exposes a client-side readiness signal (data.conv_real_backend_active).
            # enable_real_faunamls is a no-op QUERY there — called only to WAIT for the
            # real session to come up before the test drives device B; the async
            # `MlsSyncLauncher` restore still runs in the login task, so the test polls
            # the snapshot for the restored thread after this returns.
            device_b.conversations.enable_real_faunamls()
        # macOS: no enable/readiness command exists (launch-time gate — see
        # docstring); the consuming test polls the restored thread snapshot.
        yield device_b, driver
    finally:
        try:
            driver.screenshot(f"teardown-{screenshot_tag}")
        except Exception:
            pass
        driver.teardown()


@pytest.fixture
def alice_second_device(request, test_user, nest_instance):
    """A SECOND real-engine GUI app (**linux, macOS, windows, or tui**, matching
    the ``app`` param) logged in as the SAME identity as alice (``test_user`` /
    ``logged_in_app``) — her **second device**: the same 32-byte secret key (⇒
    same ``ActorId`` and same ``BackupKey``), a DISTINCT ``device_id``, and —
    crucially — a fresh, EMPTY per-launch client state root, so it boots with an
    **empty** MLS state db and MUST reconstruct its conversations by restoring
    the cross-device MLS state replica on login (``docs/goal/behavior/devices.md``
    § Cross-device MLS group-state sync — linux: the ``conv_backend.rs``
    ``wire_mls_state_sync`` leg, slice 5 leg 1; macOS + windows: the shared
    FFI-factory ``MlsSyncLauncher`` leg, ``libs/fauna-ffi/src/mls_sync_launch.rs``;
    tui: the shared tokio launcher consumed directly,
    ``apps/fauna-tui/src/conversations/conv_backend.rs``).

    Per-app isolation mechanics differ:

    * **linux / tui** — the driver's per-launch ``mkdtemp`` HOME/XDG is already a
      fresh state root (``drivers/linux.py`` / ``drivers/tui.py``); nothing extra
      to do.
    * **macOS** — every launch sets ``CFFIXED_USER_HOME`` (the store isolation
      seam the version-skew at-rest leg proved; unconditional
      since the fleet flip — ``drivers/macos.py::launch``), so device B's fresh
      default launch already gets its own empty Application Support ⇒ an empty
      MLS store. Pinning ``config["home"]`` still names the root explicitly
      where this fixture wants to own it.
      The real conversations session comes from the LAUNCH-TIME
      ``FAUNA_E2E_REAL_CONVERSATIONS`` gate (the consuming test must carry the
      ``real_conversations`` marker), so no ``enable_real_faunamls`` call —
      apple has no such command; readiness rides the consuming test's polling.
    * **windows** — every launch is already isolated (the driver defaults
      ``data_dir`` to a per-instance mkdtemp — e2e rule 10), but device B must not
      merely be *some other* dir: it must be a dir this fixture controls, because
      the point is an EMPTY ``mls.db`` device A never wrote to. Pinning
      ``config["data_dir"]`` makes the driver set ``FAUNA_E2E_DATA_DIR`` (the
      app-side ``BackupPaths.DataDir`` override the version-skew at-rest leg
      proved, ``drivers/windows.py`` / ``helpers/skew_client.py``), relocating
      the whole data dir to a fresh empty root ⇒ empty ``mls.db``. Like macOS the
      real session comes from the LAUNCH-TIME ``FAUNA_E2E_REAL_CONVERSATIONS``
      gate; UNLIKE macOS it exposes ``data.conv_real_backend_active``, so
      ``enable_real_faunamls`` is called here purely as a readiness POLL (a no-op
      query on windows) before the test drives device B.

    Built directly (NOT the session ``_driver_cache``, which holds device A),
    mirroring ``second_real_faunamls_app`` but with alice's identity instead
    of a distinct peer. **Request it via ``request.getfixturevalue`` *after* device
    A has posted + uploaded its replica**, so the on-demand login restore finds a
    non-empty replica (a plain fixture param would launch it before A posts, and it
    would restore nothing). Yields ``(device_b_app, driver)``.

    The web second-device leg is ``alice_second_web_device``; android gets its
    leg on its own machine.
    Skips under any other ``app`` param."""
    # Device A (the parametrized app) picks the leg under test.
    app = request.getfixturevalue("app")
    if app.driver.is_linux():
        app_name = "linux"
    elif app.driver.is_macos():
        app_name = "macos"
    elif app.driver.is_windows():
        app_name = "windows"
    elif app.driver.is_tui():
        # tui: like linux, the driver's per-launch fresh XDG dirs are already a
        # fresh state root (empty `mls_state.db`, `drivers/tui.py`), and the
        # real backend is a runtime toggle. The leg under test is the SHARED
        # tokio launcher (`fauna_client_mls_sync::launcher::tokio_launcher`,
        # the same one the FFI factory injects), wired by tui's
        # `conv_backend.rs`.
        app_name = "tui"
    else:
        pytest.skip(
            "cross-device MLS-sync GUI proof runs on linux (slice 5 leg 1), "
            "macOS (the native FFI-factory leg), windows (the same shared "
            "FFI-factory leg), and tui (the shared tokio launcher consumed "
            "directly); the web leg is alice_second_web_device, android "
            "pending on its machine"
        )

    yield from _alice_extra_seat(
        app_name, request, test_user, nest_instance,
        screenshot_tag="alice-second-device",
    )


class _BuilderSeats:
    """What ``alice_builder_seats`` yields: ``launch(tag)`` starts a desktop seat
    beside the phone and returns ``(builder_app, driver)``; ``retire(app)``
    quits the seat whose ``builder_app`` that is (the pair itself is accepted
    too), so a later ``launch`` is a builder coming online AGAIN."""

    def __init__(self, launch, retire, launched):
        self.launch = launch
        self.retire = retire
        self._launched = launched

    def launched_drivers(self):
        """Every seat's driver launched this test, retired ones included — the
        on-failure log hook's view of drivers no fixture value names
        (`helpers/app_log_section.py::launched_drivers`)."""
        return [driver for _, driver in self._launched]


@pytest.fixture
def alice_builder_seats(request, test_user, nest_instance):
    """DESKTOP seats of alice for a PHONE app under test, launched on demand one
    at a time — the phone's real shape (`content-index.md` § Build vs. query:
    "a desktop app … builds and syncs; phones receive the segments and query the
    synced copy"), including the shape where a builder comes online AGAIN.

    A phone never builds an index (`CLIENT_BUILDS_INDEX`), so a phone journey
    whose hit is alice's own content needs another seat to have indexed it. And
    a desktop seat learns of another seat's drafts only at its own launch
    (`reserved-folders.md` § Drafts Sync — propagation is load-on-launch, never
    push-on-write), so a journey whose content CHANGED on the phone needs a
    builder to launch after the change. Yields ``_BuilderSeats``: every seat
    ``launch`` starts is the machine's desktop app logged in as the same
    identity with a fresh state root, beside the running phone
    (``logged_in_app``), and is ``_alice_extra_seat``'s (so the isolation
    mechanics are ``alice_second_device``'s); every seat still live is torn
    down at the end.

    **Declare it in the test's signature, never ``getfixturevalue`` it.** It
    launches nothing until ``launch()`` is called, so the launch can still
    follow the phone's uploads — and only a declared fixture is in the item's
    closure, which is what lets the prebuild hoist (`_CROSS_APP_FIXTURE_APPS`)
    build the desktop app outside the test's own timeout budget.

    **The phone's identity, when it is not alice's.** A journey that logs the
    phone in as ANOTHER actor — a dedicated seeded user, or a dedicated nest's
    admin — passes that ``user`` (a ``_make_user``-shaped dict: ``signing_key``,
    ``actor_id_hex``, optional ``handle``) and its ``nest`` to ``launch``, so the
    builder is a seat of the account the phone is actually running; both default
    to alice on the session nest.

    iOS pairs with macOS, the desktop app on the machine the simulator runs on.
    Any other app under test skips — android's pair would be a desktop seat on
    the emulator's host, which no fixture launches yet."""
    app = request.getfixturevalue("app")
    if not app.driver.is_ios():
        pytest.skip(
            "the desktop builder seat pairs with a phone under test; iOS pairs "
            "with macOS — android's pairing is not built"
        )
    live: list = []  # (generator, seat), in launch order
    launched: list = []  # every seat ever launched, for the on-failure log

    def launch(tag="alice-builder-seat", *, user=None, nest=None):
        gen = _alice_extra_seat(
            "macos", request, user or test_user, nest or nest_instance,
            screenshot_tag=tag,
        )
        seat = next(gen)
        live.append((gen, seat))
        launched.append(seat)
        return seat

    def retire(seat):
        for i, (gen, launched) in enumerate(live):
            if launched is seat or launched[0] is seat:
                del live[i]
                # Runs the launcher's own `finally`: screenshot, then teardown.
                gen.close()
                return
        raise ValueError("retire: not a live builder seat")

    try:
        yield _BuilderSeats(launch, retire, launched)
    finally:
        while live:
            gen, _ = live.pop()
            gen.close()


@pytest.fixture
def alice_second_web_device(request, test_user, nest_instance):
    """A SECOND live **web** page logged in as the SAME identity as alice
    (``test_user`` / ``logged_in_app``) — her **second device**, CONCURRENT with
    device A: the web twin of ``alice_second_device``. The twin page lives in
    its OWN Playwright ``BrowserContext`` (``WebBridgeDriver.open_twin_page``)
    — isolated localStorage — so it boots a fresh, empty in-memory MLS engine
    under the same secret (⇒ same ``ActorId`` + ``BackupKey``) and MUST
    reconstruct its conversations by restoring the cross-device MLS state
    replica on its manager build (``docs/goal/behavior/devices.md`` § Cross-device
    MLS group-state sync — slice 6 sub-part (a): two live tabs behave as two
    devices; there is no single-tab guard on the conversations plane).

    **Request it via ``request.getfixturevalue`` *after* device A has posted +
    uploaded its replica**, so the twin's login restore finds a non-empty
    replica. Yields ``(device_b_app, twin_driver)``. Web-only (the linux twin
    is ``alice_second_device``); skips under a non-web ``app`` param."""
    app = request.getfixturevalue("app")
    if not app.driver.is_web():
        pytest.skip(
            "the concurrent second-web-device fixture validates the web "
            "conversations plane (slice 6 sub-part a); linux's twin is "
            "alice_second_device"
        )

    secret_hex = test_user["signing_key"].encode().hex()
    twin_driver = app.driver.open_twin_page()
    try:
        twin_driver.set_state({
            "session": {
                "authenticated": True,
                # The SPA proxy URL, as in `_login_app_as` for web (the browser
                # fetches relative to it).
                "node_url": request.getfixturevalue("spa_url"),
                "secret_hex": secret_hex,
                # The same actor's REAL nest handle as device A (see
                # `_login_app_as`) — a second device of one identity must not
                # report a different one.
                "handle": test_user.get("handle") or "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                # A DISTINCT device id from device A's `_E2E_LOGIN_DEVICE_ID`:
                # same identity, different device (mirrors alice_second_device).
                "device_id": "fedcba9876543210" * 4,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        device_b = ActionLayer(twin_driver)
        device_b.conversations.enable_real_faunamls()
        yield device_b, twin_driver
    finally:
        try:
            twin_driver.screenshot("teardown-alice-second-web-device")
        except Exception:
            pass
        twin_driver.teardown()


@pytest.fixture
def caldav_mailbox_less_attendee_app(request, app, dedicated_caldav_only_nest):
    """A SECOND real-engine app, **client-matched to the selected app**
    (linux/tui — the ``_launch_second_real_faunamls_app`` idiom, joined by
    macos/ios row 283 and windows row 168), logged in as a MAILBOX-LESS
    attendee (carol) on the **CalDAV-only** nest — the Slice C
    materialize-via-GUI harness (``caldav-server.md`` § Server-side auto-schedule;
    Slice C). carol's seat is what the events-page assertion renders on, so a
    hardcoded seat would let another column claim a witness that never ran
    there; every app it runs on carries the whole path (the
    ``enable_caldav_mailbox`` agent command, the real ``ConversationsSession``,
    the events page's calendar poll).

    carol is a **registered handled actor** (``carol@<domain>``) — her handle the
    MDA's ``fauna.actor.by_handle`` resolves — but with **no mail alias**, so she
    is genuinely mailbox-less (the MDA gateway classifies her onto the sealed
    scheduling rail, not the email rail). Her real conversations engine publishes
    her key packages at login (``ConversationsSession::start_receive_loop replenish``, holding the
    private half) so the MDA-sealed Scheduling welcome decrypts, and runs the
    scheduling-drain receive loop. The test then mints her MSEK (the
    ``enable_caldav_mailbox`` test command) and asserts the sealed iMIP REQUEST
    materializes on her Events page — the GUI mirror of the conformance twin's
    drain+apply half.

    Native-only: the web SPA has no mailbox-less rail yet, and android has no
    second-seat harness here — any other selected app skips. Builds a fresh
    driver **directly** (NOT the session ``_driver_cache``, which is the
    organizer ``app``), logs carol in via ``set_state`` (mirroring
    ``second_real_faunamls_app``), then activates her real engine (on tui the
    call is a readiness probe — ``conv_backend.rs`` builds the real session
    unconditionally at login, the ``tui_member`` shape; windows is likewise a
    launch-gated readiness probe, same as ``real_faunamls_app``'s own
    windows branch). Yields
    ``(carol_app, carol_actor)`` where ``carol_actor`` is her
    ``register_handled_actor`` dict (signing key / actor id / token / handle).
    """
    from common.auth import register_handled_actor

    if app.driver.is_linux():
        app_name = "linux"
    elif app.driver.is_tui():
        app_name = "tui"
    elif app.driver.is_macos():
        # row 283: apple's per-launch isolation (e2e-conventions.md point 10)
        # gives a second, independently-authenticated app process for free —
        # same harness shape as linux/tui, just a different app_name.
        app_name = "macos"
    elif app.driver.is_ios():
        app_name = "ios"
    elif app.driver.is_windows():
        # row 168: windows' per-launch isolation (FAUNA_E2E_CREDENTIAL_DIR +
        # FAUNA_E2E_DATA_DIR, `drivers/windows.py`) gives a second,
        # independently-authenticated app process for free — same harness
        # shape as apple, just a different app_name.
        app_name = "windows"
    elif app.driver.is_web():
        # row 67: web's second seat is a twin PAGE in its own BrowserContext
        # (`alice_second_web_device`'s idiom), not a second driver — isolated
        # localStorage is exactly the per-seat isolation the native arms get from
        # a separate process, and a browser is far too heavy to launch twice.
        # carol reaches the CalDAV-only nest through that nest's own SPA proxy
        # (`spa_proxy_for`); the raw nest URL is unreachable from a browser origin.
        # Her drain is `pollScheduling` on the web receive tick — the web leg of
        # the mailbox-less rail (`caldav-server.md` § Server-side auto-schedule,
        # Half-1), which is what this arm exists to witness.
        app_name = "web"
    else:
        pytest.skip(
            "the mailbox-less attendee seat runs on the direct-Rust clients "
            "(linux + tui), apple (macos + ios), windows and web; android has "
            "no second-seat harness here"
        )

    handle = dedicated_caldav_only_nest
    nest = handle.nest
    domain = handle.domain

    # carol: a registered handled actor on the dedicated nest — handle resolvable
    # by `by_handle`, but no alias (mailbox-less). NO manual key-package upload —
    # her real engine publishes its own at login (private half held), so the
    # MDA-sealed welcome is decryptable; an API-uploaded throwaway package would
    # not be.
    carol = register_handled_actor(nest["port"], handle="carol", domain=domain)

    if app_name == "web":
        # The twin page owns no process, so there is no launch config and nothing
        # to tear down but the page itself (handled below).
        driver = app.driver.open_twin_page()
        node_url = request.getfixturevalue("spa_proxy_for")(nest["url"])
    else:
        config = _build_app_config(app_name, nest, request)
        if app_name == "ios":
            # carol is a SECOND, concurrently-live iOS app in this session — she
            # cannot share the organizer's `_get_ios_setup` device (
            # launch()'s own uninstall+install would tear the organizer's app
            # down mid-test — `drivers/ios.py:125-127, 547-551`).
            second_udid = _get_ios_second_seat_udid()
            if second_udid is None:
                pytest.skip("could not allocate a second iOS simulator for carol's seat")
            config["udid"] = second_udid

        driver = create_driver(app_name)
        driver.launch(config)
        node_url = nest["url"]
    try:
        secret_hex = carol["signing_key"].encode().hex()
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "carol",
                "actor_id": carol["actor_id_hex"],
                "device_id": "test-device-carol",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        carol_app = ActionLayer(driver)
        # Activate carol's real engine: publishes her key packages + spawns the
        # scheduling-drain receive loop, so the MDA-sealed welcome is drained +
        # decrypted by the real MLS engine.
        carol_app.conversations.enable_real_faunamls()
        yield carol_app, carol
    finally:
        try:
            driver.screenshot("teardown-caldav-mailbox-less-carol")
        except Exception:
            pass
        driver.teardown()


def _launch_fresh_share_driver(app_name: str, nest: dict, request):
    """Create + launch a driver for the folder Sharing cross-user fixtures below,
    on the standard launch config for ``nest`` (``_build_app_config``, which seeds
    the app's escrow trust in that nest), applying the SAME render-readiness
    preflight ``_driver_cache`` applies to its
    cached apple driver — a fresh driver gets no free pass just because it isn't
    cached; an apple process can come up fully "healthy" (port listening, ``/health``
    200) with no WindowServer-backed window, and every element query would then
    time out looking exactly like a missing-feature red.
    """
    from drivers.inprocess_agent import InProcessAgentDriver, RenderNotReady

    config = _build_app_config(app_name, nest, request)
    _own_windows_share_seat_agent(app_name, config, request)
    driver = create_driver(app_name)
    driver.launch(config)
    if isinstance(driver, InProcessAgentDriver):
        try:
            driver.assert_render_ready()
        except RenderNotReady as exc:
            try:
                driver.teardown()
            except Exception:
                pass
            pytest.exit(
                f"apple in-process render preflight FAILED for '{app_name}': {exc}",
                returncode=3,
            )
    return driver


def _own_windows_share_seat_agent(app_name: str, config: dict, request) -> None:
    r"""Give a fresh windows share seat its OWN sync agent pipe + state dir.

    The session-wide agent pin (:func:`_apply_isolated_sync_agent_env`) names ONE
    pipe per pytest session — right for the session-scoped cached app, wrong for
    the fresh seats this module builds: two windows seats in one run (a windows
    owner sharing to a windows member) would rendezvous on the same agent, and
    the LAST login to provision it wins. Measured 2026-09-29 on
    ``test_folder_member_media_decrypt``'s windows × windows arm: the member's
    app re-provisioned the owner's agent with the member's identity, so the
    owner's bound folder ran its engine as an actor that cannot see the folder
    (``fauna.folders.not_found`` → "content-key binding indeterminate", fail
    closed) and the clip never uploaded. Convention 10's per-launch isolation,
    the shape :func:`helpers.sync_seats.isolate_windows_sync_agent` gives the
    multiseat pair; the unix apps get it for free from their private HOME.

    Only the pipe and the data dir move: whether an agent runs at all (the
    session posture and its markers) and which binary is pinned stay the
    session's. Each seat's bridge reaps its own tree, the agent included
    (``drivers/windows.py`` ``_start_bridge``), so nothing outlives the seat.
    """
    if app_name != "windows":
        return
    env = config.get("environment") or {}
    if not env.get("FAUNA_E2E_SYNC_PIPE"):
        return  # no agent this run (`no_sync_agent`, or the per-SID journey)
    from helpers.sync_seats import windows_agent_data_dir

    root = request.getfixturevalue("tmp_path_factory").mktemp("share-seat")
    agent_dir = windows_agent_data_dir(root)
    agent_dir.mkdir(parents=True, exist_ok=True)
    env["FAUNA_E2E_SYNC_PIPE"] = f"fauna-sync-e2e-{os.getpid()}-{root.name}"
    env["FAUNA_E2E_SYNC_AGENT_DATA_DIR"] = str(agent_dir)


# ── the second tui seat (lifted from test_folder_member_media_decrypt.py,
#    2026-08-21, when a second module needed the same seat) ──────────────

@pytest.fixture()
def tui_member(request, handled_nest, tui_app_path, tmp_path):
    """A fresh **tui** launch logged in as a handled actor on ``handled_nest``,
    with the real conversations session live and the external-open handoff seams
    pinned into its environment.

    The tui twin of ``folder_share_recipient_app`` (which is
    linux/macOS/iOS/web) — a separate fixture rather than another param on that
    one because this recipient needs two things at *launch* time that the shared
    fixture has no seam for: ``FAUNA_TUI_MEDIA_OPENER`` (the OS-handler test
    seam) and a private ``XDG_RUNTIME_DIR``/``TMPDIR`` under which the ratified
    ``<base>/fauna-tui/media`` handoff file must land. Adding "tui" to the shared
    fixture's params would also silently enrol every existing consumer of it into
    a tui run their UI cannot serve.

    ``handled_nest`` (not ``nest_instance``) so the member's handle is
    ``by_handle``-resolvable for the owner's share gesture, and so the receive
    rail is handle-enabled. tui builds its real ``ConversationsSession`` (MLS
    engine + login-time KeyPackage publish + the receive loop + the folder gate
    + the custody sink) unconditionally at login — ``conv_backend.rs`` — so the
    ``enable_real_faunamls`` call below is a readiness *probe*, not an
    activation.

    Yields ``(app, actor, record_file, runtime_dir)``.
    """
    if "tui" not in get_available_apps():
        pytest.skip("fauna-tui is not available on this machine")

    import secrets

    from common.auth import register_handled_actor

    actor = register_handled_actor(
        handled_nest["port"],
        handle="member" + secrets.token_hex(3),
        domain=MAIL_PRIMARY_DOMAIN,
    )

    record_file = tmp_path / "opener-record.txt"
    runtime_dir = tmp_path / "member-runtime"
    runtime_dir.mkdir(mode=0o700)
    # "A file the OS will execute" is per-OS, exactly as in fauna-tui's own
    # `os_open::spawn_via_launches_the_opener_with_the_url`: a `#!` script with
    # the exec bit on unix, a `.cmd` batch on windows. A `.sh` is not executable
    # there at all, so tui's fire-and-forget spawn failed silently and nothing
    # was ever recorded — every external-open witness on Windows read as a failed
    # decrypt (found running test_folder_member_media_decrypt.py there,
    # 2026-09-25).
    if sys.platform == "win32":
        opener = tmp_path / "fake-opener.cmd"
        opener.write_text(f'@echo off\r\n(echo %~1)>>"{record_file}"\r\n')
    else:
        opener = tmp_path / "fake-opener.sh"
        opener.write_text(f'#!/bin/sh\nprintf \'%s\\n\' "$1" >> "{record_file}"\n')
        opener.chmod(0o755)

    driver = create_driver("tui")
    driver.launch(
        {
            "app_path": tui_app_path,
            "url": handled_nest["url"],
            "environment": {
                **_seeded_environment(request, handled_nest),
                "FAUNA_TUI_MEDIA_OPENER": str(opener),
                # This launch passes an explicit env dict, so it does NOT inherit
                # `_apply_real_conversations_env`'s `FAUNA_CONV_POLL_SECS=2` and
                # would otherwise run the 30 s production backstop
                # (`DEFAULT_CONV_POLL_SECS`). Set to 2 to match every other
                # real-conversations launch. ⚠ NOT muteable the way D9's
                # conversation-rail fixture was: `poll_folder_feed` (the initial
                # Welcome/custody join for BOTH tests, not just the rotation
                # one) has NO push arm at all — `ConversationsPush` only carries
                # `welcome.received`/`channel.message` (`session.rs:1551`), so
                # folder shares are ticker/poke-driven exclusively. Muting
                # this to a large value broke the initial join outright
                # (measured 2026-08-14: `_await_member_row` timed out at 120s
                # with an empty `fauna.media.list`). The post-rotation
                # re-ingest below IS poke-anchored now; the initial
                # join staying tick-dependent is out of that row's scope, not
                # an oversight.
                "FAUNA_CONV_POLL_SECS": "2",
                # The ratified handoff base is per-OS (apps/tui.md § External
                # media handoff → Location): Linux resolves $XDG_RUNTIME_DIR,
                # macOS and Windows the per-user temp dir (TMPDIR there; TMP,
                # then TEMP, on Windows). Pin all of them so the assertion below
                # stays OS-branch-free and never touches the real runtime dir.
                "XDG_RUNTIME_DIR": str(runtime_dir),
                "TMPDIR": str(runtime_dir),
                "TMP": str(runtime_dir),
                "TEMP": str(runtime_dir),
                # The tui's tracing subscriber honours `RUST_LOG` and defaults
                # to `info` (`session.rs::install_logging`), at which the whole
                # receive-loop / custody / media path is silent — a diagnosis
                # run came back with only the mls-sync launcher line. Under the
                # driver stderr is a plain FILE (not the terminal), so the
                # stderr layer is live and `app_stderr_text()` can read it.
                # Scoped to the crates this test's failure modes live in rather
                # than a blanket `debug`, which would drown them in transport
                # frames.
                "RUST_LOG": (
                    "info,fauna_media=debug,fauna_media_machine=debug,"
                    "fauna_conversations=debug,fauna_client_folders=debug,"
                    "fauna_tui=debug"
                ),
            },
        }
    )
    try:
        driver.set_state(
            {
                "session": {
                    "authenticated": True,
                    "node_url": handled_nest["url"],
                    "secret_hex": actor["signing_key"].encode().hex(),
                    "handle": actor["handle"],
                    "actor_id": actor["actor_id_hex"],
                    "device_id": _E2E_SHARE_RECIPIENT_DEVICE_ID,
                },
                "nav": {"stack": [{"view": "feed"}]},
            }
        )
        app = ActionLayer(driver)
        # Wait until the login-built session is live: its receive loop is what
        # ingests the Welcome and the custody bundle, and its KeyPackage publish
        # is what the owner's share fetches to admit the member.
        app.conversations.enable_real_faunamls()
        yield app, actor, record_file, runtime_dir
    finally:
        try:
            driver.teardown()
        except Exception:
            pass


@dataclass(frozen=True)
class MediaMemberSeat:
    """One shared-folder MEMBER seat for the Media read witnesses, whichever app
    holds it — the shape ``media_member`` hands the test. Every seat reads
    through the ``media-item-detail-download-button`` download (web's captured
    browser download, a native app's file in ``driver.download_dir()``), so the
    seat carries no app-specific observation seam.
    """

    app: ActionLayer
    actor: dict


@pytest.fixture()
def media_member(request):
    """The member seat of the Media read witnesses, over ``request.param`` —
    any app, each reading through the ``media-item-detail-download-button``
    download (android's seat launches, but its driver has no download
    read-back yet). A test
    parametrizes it INDIRECTLY, each arm carrying its app mark
    (``pytest.param("web", marks=pytest.mark.web)``), so the run's app axis
    selects arms exactly as it does for ``folder_share_owner_app``; it is in
    ``_REAL_SECOND_APP_FIXTURES`` because it IS a second real seat — both seats
    must be drivable on this machine, an any-of match would let an impossible
    arm through collection.

    The download needs no launch-time seam (the file lands where the driver
    reads it back — ``WebDriver.download_via_click``, or a native app's
    dialog-less save into ``driver.download_dir()``), so a non-tui seat is the
    ordinary recipient seat (``_folder_share_recipient_seat``) under a
    member-shaped handle; tui's is ``tui_member``, whose opener and handoff-dir
    seams the download simply leaves unused. The
    recipient's receive loop is the same shared ``NestFolderGate`` +
    ``NestFolderCustodySink`` on every app, so a contact's share auto-joins and
    its custody lands in the member's ``fauna.state.folder-keys`` plane exactly as on tui; what the
    Media page then does with that custody is the witness's job.
    """
    seat = request.param
    if seat == "tui":
        app, actor, _record_file, _runtime_dir = request.getfixturevalue("tui_member")
        yield MediaMemberSeat(app, actor)
        return
    if seat not in get_available_apps():
        pytest.skip(f"fauna-{seat} is not available on this machine")
    handled_nest = request.getfixturevalue("handled_nest")
    for app, actor in _folder_share_recipient_seat(
        seat, handled_nest, request,
        handle_prefix="member", screenshot=f"teardown-{seat}-member",
    ):
        yield MediaMemberSeat(app, actor)



@pytest.fixture(params=["linux", "macos", "ios", "web", "tui", "windows"])
def folder_share_owner_app(request, handled_nest):
    """A FRESH GUI app (linux / macOS / iOS / web / tui / windows, per ``request.param``) logged in as
    a handled OWNER on ``handled_nest``, with a LIVE conversations session (its real
    MLS engine, key packages published at login) so ``FoldersAuthor::share_set`` can
    admit a recipient to a fresh MLS group. The home for the owner-side full share
    round-trip (``folders.md`` § Sharing — *Owner side*); the recipient is seeded
    headlessly on the same nest.

    Built as a fresh driver **directly** (NOT the single-app session
    ``_driver_cache``): the cached ``app``'s conversations session is a process-static
    ``ACTIVE_SESSION`` built at its FIRST ``AuthSuccess``, and the launch guard blocks
    a 2nd auth — so re-pointing the cached app at ``handled_nest`` via ``set_state``
    (as ``handled_logged_in_app`` does) leaves the session on the previously-authed
    nest, and the share resolves/binds against the wrong nest (empty roster, no error;
    the ``set_state``-does-not-reauth-running-app trap). A fresh driver = fresh process
    = a session built against ``handled_nest`` on first auth. Mirrors
    ``caldav_mailbox_less_attendee_app`` / ``second_real_faunamls_app`` (linux
    has no Chromium lock, so a second linux app coexists with the cached one).
    ``handled_nest`` (not the handle-less ``nest_instance``) so the seeded recipient's
    handle is ``by_handle``-resolvable.

    **Generalized from a linux-only fixture (2026-07-12).** linux activates the real
    conversations backend at RUNTIME (``enable_real_faunamls()``, a bridge command
    that flips its mock→real and waits for the receive loop); apple has no such
    command — its native apps default to the mock backend and opt into the real
    ``ConversationsSession`` (which publishes login-time KeyPackages exactly like
    linux's, in the SAME shared-Rust ``start_receive_loop`` — see
    ``libs/fauna-conversations/src/session.rs``) only under a LAUNCH-TIME env gate,
    ``FAUNA_E2E_REAL_CONVERSATIONS=1``, applied automatically by
    ``_build_app_config``'s ``_apply_real_conversations_env`` when a selected test
    carries the ``real_conversations`` marker. So the macOS/iOS branch below skips the
    linux-only activation call entirely; the consuming test supplies the marker.

    ⚠ **apple SQLite-contention watch item (downgraded from a hard hazard after
    live verification 2026-07-12) — read before combining this with other apple
    tests in the same invocation.** ``FAUNA_E2E_REAL_CONVERSATIONS`` is
    session-wide (any selected test carrying the marker flips EVERY apple config
    built that session, including the session-cached ``app``/``logged_in_app``
    driver other tests use). Unlike linux (no such lock), the native apps' real
    conversations rail opens ONE SQLite store per signed-in account (``~/Library/
    Application Support/Fauna/<actor-id-hex>/mls.db`` on macOS; account-scoped
    since 2026-07-22, `account-scoping.md`) — two processes signed in as the SAME
    account still share it, so the hazard is unchanged: the session-cached
    macOS driver and this fixture's fresh second process both end up with real
    conversations active and open that same file concurrently. **Ran the full
    ``test_folders.py --client macos`` (cached-app tests + both of these
    fixture's tests, combined, in one invocation) live and all 12 passed** — so
    SQLite's own locking tolerates this in practice, at least at this
    concurrency level; it is NOT a proven hard blocker. Still worth watching:
    if a flaky "database is locked"-shaped failure shows up on a future
    combined run, this shared-file contention is the first thing to suspect —
    fall back to an isolated invocation (e.g. ``pytest test_folders.py -k
    folder_full_share_round_trip --client macos``) to rule it out (tracked
    internally, § Gotchas).

    **windows joined 2026-08-10, and the thing that had kept it out is gone.**
    The standing objection was namespace collision, not a missing surface: two
    windows ``FaunaApp`` processes used to share ONE OS credential store (the
    ``PasswordVault`` finding), so a fresh second app could not hold its own
    identity. Both halves of that are now per-launch — ``drivers/windows.py``
    sets ``FAUNA_E2E_CREDENTIAL_DIR`` (→ ``CredentialStore.cs``'s
    ``FileSecretBackend``, the deliberate twin of linux's file store, instead of
    the vault) and ``FAUNA_E2E_DATA_DIR`` (→ ``BackupPaths.DataDir``, which
    relocates ``mls.db`` too). So windows takes the apple-shaped LAUNCH-gated
    branch below rather than linux's runtime activation call: its conversations
    session is built for EVERY e2e login (``App.xaml.cs``
    ``BuildE2eConvSessionAsync`` — without it every ``_convSession``-gated
    folder gesture silently no-ops), and the receive loop + login-time
    KeyPackage publish that the share crypto needs are what
    ``FAUNA_E2E_REAL_CONVERSATIONS`` (the ``real_conversations`` marker) turns
    on. ⚠ ``driver.recover()`` is NOT a second device on windows (same data dir
    = same-device restart) — a genuine second seat is a second driver with its
    own pinned ``data_dir``, which is exactly what this fixture builds.

    Yields ``(owner_app, handled_nest, owner)`` where ``owner`` is the
    ``register_handled_actor`` dict — symmetric with
    ``folder_share_recipient_app``, which has always yielded its actor. The
    owner is what lets a consumer read the NEST-side roster
    (``conv_api.folder_member_actors``, owner-gated) as ground truth: without
    it a red on the UI roster count cannot tell "the share never ran" from "it
    ran and the row did not repaint", and on at least one app its own
    ``error-message`` stays empty across that whole fork.
    """
    import secrets

    from common.auth import register_handled_actor

    app_name = request.param
    owner = register_handled_actor(
        handled_nest["port"], handle="owner" + secrets.token_hex(3),
        domain=MAIL_PRIMARY_DOMAIN,
    )
    driver = _launch_fresh_share_driver(app_name, handled_nest, request)
    try:
        secret_hex = owner["signing_key"].encode().hex()
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": handled_nest["url"],
                "secret_hex": secret_hex,
                "handle": owner["handle"],
                "actor_id": owner["actor_id_hex"],
                "device_id": _E2E_LOGIN_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        owner_app = ActionLayer(driver)
        if app_name in ("linux", "web", "tui"):
            # First auth for THIS process → AuthSuccess builds the conversations
            # session (real MLS engine + login-time key-package publish) against
            # handled_nest; this waits until it's live so share_set has a ready
            # engine. apple/windows instead gate this at LAUNCH (see docstring).
            #
            # web joined 2026-09-20 — a pure capability
            # add: the body above is already app-agnostic, and the INVERSE fixture
            # `folder_share_recipient_app` has run its own web arm green through
            # this same `_launch_fresh_share_driver("web", …)` path since
            # 2026-08-30. web takes linux's explicit-activation branch for the
            # reason written out there: its `conversations_enable_real_faunamls`
            # handler re-registers the real `FaunaMlsBackend` on the manager
            # singleton and fires the login-time keypackage replenish that
            # admitting a recipient depends on. The raw `handled_nest["url"]`
            # above is correct for web too — that nest seeds the SPA proxy's
            # origin into its `cors_origins`, which is why the browser may dial
            # it round the proxy (see the browser-origins fixture's docstring).
            #
            # tui is linux's deliberate twin here, NOT an apple-style launch gate:
            # it always builds the real `ConversationsSession` at AuthSuccess
            # (`conv_backend::start_conversations_session`), and its
            # `conversations_enable_real_faunamls` command is a *readiness probe*
            # over `data.conv_real_backend_active` (`automation.rs`) rather than a
            # mock→real flip — so the call means the same thing it does on linux
            # ("block until the engine is live") and needs no new agent command.
            owner_app.conversations.enable_real_faunamls()
        yield owner_app, handled_nest, owner
    finally:
        try:
            driver.screenshot("teardown-folder-share-owner")
        except Exception:
            pass
        driver.teardown()


@pytest.fixture(params=["linux", "macos", "ios", "web", "tui", "windows", "android"])
def folder_share_recipient_app(request, handled_nest):
    """The RECIPIENT seat of the folder Sharing cross-user fixtures, per
    ``request.param`` — the whole story is on :func:`_folder_share_recipient_seat`,
    which this yields from. Yields ``(recipient_app, handled_nest, recipient)``."""
    for recipient_app, recipient in _folder_share_recipient_seat(
        request.param, handled_nest, request
    ):
        yield recipient_app, handled_nest, recipient


def _folder_share_recipient_seat(
    app_name: str,
    handled_nest: dict,
    request,
    *,
    handle_prefix: str = "recip",
    screenshot: str = "teardown-folder-share-recipient",
):
    """A FRESH GUI app (linux / macOS / iOS / web / tui / windows / android, per ``app_name``) logged in as
    a handled RECIPIENT on ``handled_nest``, with a LIVE conversations session (real
    MLS engine + login-time KeyPackage publish, so a headless sharer can fetch one to
    admit it to a group; the receive loop drains the durable inbox + runs the
    recipient contact-gate). The home for the recipient-side pending-share round-trip
    (``folders.md`` § Sharing — *Recipient side*: "auto for contacts, knock for
    strangers"). The INVERSE of ``folder_share_owner_app``: here the driven GUI is
    the RECIPIENT and the SHARER is seeded headlessly (a stranger who mints a group
    Welcome against the recipient's published KeyPackage and delivers it as
    ``channel_type="folder"``).

    Fresh driver (NOT the cached ``app``) for the same reason the owner fixture is —
    the cached app's conversations session is process-static ``ACTIVE_SESSION`` built
    at its first ``AuthSuccess``, so ``set_state`` can't re-point it at
    ``handled_nest`` (the ``set_state``-does-not-reauth-running-app trap).
    ``handled_nest`` (not ``nest_instance``) so the recipient's handle is
    ``by_handle``-resolvable and the receive rail is handle-enabled.

    **Generalized from a linux-only fixture (2026-07-12) — see
    ``folder_share_owner_app``'s docstring for the full apple-porting rationale
    (no runtime activation call; launch-time env gate instead) and the
    MLS-store isolation hazard, which applies here identically.** The apple
    login-time KeyPackage publish (shared-Rust ``start_receive_loop``, unconditional)
    is what the headless sharer's ``keypackage_fetch`` poll below depends on — it is
    NOT linux-specific, confirmed by reading ``libs/fauna-conversations/src/session.rs``
    and apple's own login call site (``FaunaMacApp.swift`` /
    ``Fauna-iOS/App/FaunaApp.swift``), both of which build the identical
    ``ConversationsSession`` and call the identical ``startReceiveLoop()``.

    Yields ``(recipient_app, recipient)`` where ``recipient`` is the
    ``register_handled_actor`` dict (its ``actor_id_hex`` is the KeyPackage-fetch
    target the headless sharer addresses). A generator, not a fixture: the
    fixtures that hand out this seat (``folder_share_recipient_app`` per param,
    ``media_member``'s download seats for the Media member witnesses) iterate it
    so its teardown runs on theirs.
    """
    import secrets

    from common.auth import register_handled_actor

    recipient = register_handled_actor(
        handled_nest["port"], handle=handle_prefix + secrets.token_hex(3),
        domain=MAIL_PRIMARY_DOMAIN,
    )
    driver = _launch_fresh_share_driver(app_name, handled_nest, request)
    try:
        secret_hex = recipient["signing_key"].encode().hex()
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": handled_nest["url"],
                "secret_hex": secret_hex,
                "handle": recipient["handle"],
                "actor_id": recipient["actor_id_hex"],
                # DISTINCT from the owner seat's id — see the constant's own note.
                "device_id": _E2E_SHARE_RECIPIENT_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        recipient_app = ActionLayer(driver)
        if app_name in ("linux", "web", "tui"):
            # First auth for THIS process → AuthSuccess builds the conversations
            # session (real MLS engine + login-time key-package publish + the
            # receive loop + the folder contact-gate) against handled_nest; wait
            # until it's live so the recipient can be admitted to a group and
            # drains the staged Welcome. apple/windows/android gate this at
            # LAUNCH (see folder_share_owner_app's docstring; android's twin is
            # the `FAUNA_E2E_REAL_CONVERSATIONS` intent extra).
            #
            # web takes the same explicit-activation path as linux: its
            # `conversations_enable_real_faunamls` handler re-registers the real
            # `FaunaMlsBackend` on the manager singleton and fires the same
            # login-time keypackage replenish the headless sharer's
            # `keypackage_fetch` poll depends on. The web recipient gate is the
            # wasm arm of the SAME shared `ingest_welcome_by_kind` +
            # `NestFolderGate` the natives run (B5).
            recipient_app.conversations.enable_real_faunamls()
        yield recipient_app, recipient
    finally:
        try:
            driver.screenshot(screenshot)
        except Exception:
            pass
        driver.teardown()


@pytest.fixture(params=["tui", "linux"])
def folder_share_stranger_app(request, handled_nest):
    """A FRESH app logged in as a THIRD handled actor on ``handled_nest`` — a
    person nobody shares anything with. The outsider beside
    ``folder_share_owner_app`` and ``folder_share_recipient_app``: the witness
    that a non-member gets nothing readable from the owner's device
    (``p2p.md`` § Cross-user shared-set transfer) needs a seat whose identity
    the owner's roster has never heard of.

    tui and linux: the stranger's side of that journey is the compile-gated
    ``offline_share_probe_set`` agent command. A new app joins by gaining that
    command and a param here.

    No conversations activation: a stranger has no set to receive, and its
    listener is bound through the co-present panel, which needs only a signed-in
    session and the nest's ``p2p-share`` capability.

    Yields ``(stranger_app, handled_nest, stranger)``.
    """
    import secrets

    from common.auth import register_handled_actor

    stranger = register_handled_actor(
        handled_nest["port"], handle="stranger" + secrets.token_hex(3),
        domain=MAIL_PRIMARY_DOMAIN,
    )
    driver = _launch_fresh_share_driver(request.param, handled_nest, request)
    try:
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": handled_nest["url"],
                "secret_hex": stranger["signing_key"].encode().hex(),
                "handle": stranger["handle"],
                "actor_id": stranger["actor_id_hex"],
                "device_id": _E2E_SHARE_STRANGER_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        yield ActionLayer(driver), handled_nest, stranger
    finally:
        try:
            driver.screenshot("teardown-folder-share-stranger")
        except Exception:
            pass
        driver.teardown()


@pytest.fixture
def real_faunamls_linux_sender(request, logged_in_app, nest_instance):
    """A fresh real-engine **linux** GUI app (alice, the SENDER) for the
    **web** layer-5 receive harness, on the same nest as the cached web receiver.

    The web leg of layer 5 (``api-layers.md`` § Inbox & Messaging — "Durable
    inbox-apply consumer", layer 5): the receiver (bob) is
    the cached web ``logged_in_app``; the sender is a fresh **linux** driver for
    cross-app value (one real linux engine, one real web engine), built
    **directly** (NOT via the single-app session ``_driver_cache``). (A second
    web driver would also work now — bundled Chromium, no singleton — the
    linux sender predates that and stays for the cross-app coverage.)

    The twin of ``second_real_faunamls_app`` minus its push-suppression knob
    (a sender needs its push arm) and minus its coupling to ``real_faunamls_app``
    being the cached linux app: here the cached app is the **web** receiver, so
    this fixture skips on any non-web parametrization (the fresh linux driver is
    never launched). Builds a fresh ``LinuxBridgeDriver``, logs alice in as a
    *distinct* registered identity via ``set_state`` (mirroring ``logged_in_app``),
    then activates her real ``FaunaMlsBackend`` so her send is the real
    cross-engine MLS bootstrap.

    Yields ``(alice_app, alice_actor)`` where ``alice_actor`` is her
    ``create_actor_and_register`` dict (signing key / actor id / token)."""
    if not logged_in_app.driver.is_web():
        pytest.skip(
            "web layer-5 receive harness: bob is the cached web GUI, so this "
            "linux sender only runs under --client web (native two-GUI drain "
            "proof is test_fauna_mls_two_client_inbox_drain.py)"
        )
    if "linux" not in sweep_apps():
        # An app-parametrization gate alone (above) is not a machine-capability
        # gate: on a `[web]` param this unconditionally tries to build+launch a
        # LINUX driver, which fails on a box with no linux toolchain (e.g. Windows)
        # instead of skipping cleanly — the exact bound-inversion convention 9's
        # self-termination guarantee exists to prevent. `_prebuild_binaries`
        # already bounds its own cross-app hoist by `sweep_apps()` the same way,
        # for the same reason (this file's own comment on that hoist, above).
        from helpers.app_surface import skip_environment
        skip_environment(
            "this box's sweep set has no 'linux' (no linux build toolchain) — "
            "the fresh linux sender this fixture launches cannot run here"
        )

    # alice: a distinct registered actor on the SAME nest as the web receiver.
    admin_sk = nest_instance["admin"]["signing_key"]
    alice = create_actor_and_register(
        nest_instance["port"], admin_signing_key=admin_sk
    )

    # Reuse the standard linux launch config (app_path + the shared
    # FAUNA_CONV_POLL_SECS=2 fast cadence). No push suppression — a sender keeps
    # its push arm; this is the receiver's poll-only constraint, not the sender's.
    config = _build_app_config("linux", nest_instance, request)

    driver = create_driver("linux")
    driver.launch(config)
    try:
        secret_hex = alice["signing_key"].encode().hex()
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": secret_hex,
                "handle": "alice-e2e",
                "actor_id": alice["actor_id_hex"],
                "device_id": "test-device-alice",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        alice_app = ActionLayer(driver)
        # Activate alice's real engine (replaces the e2e mock) so her send is the
        # real MLS group bootstrap, not a mock; wait until the backend is live.
        alice_app.conversations.enable_real_faunamls()
        yield alice_app, alice
    finally:
        try:
            driver.screenshot("teardown-linux-sender-alice")
        except Exception:
            pass
        driver.teardown()


def _serve_spa_proxy(static_dir, nest_url, abort_once_paths=None):
    """Serve the built Svelte SPA on a local HTTP port, proxying ``/api/`` +
    ``/admin/`` (HTTP and the WebSocket upgrade) to ``nest_url``.

    The SPA is built with base="/app", so files are served under /app/. Returns
    ``(base_url, server)``; the caller shuts ``server`` down on teardown.

    Shared by ``spa_url`` (→ the session ``nest_instance``) and
    ``dedicated_mail_spa_url`` (→ a function-scoped dedicated mail nest). A web
    browser can only reach a nest through such a proxy: it adds the
    ``Access-Control-Allow-Origin`` the raw nest never sends, so the WASM
    WS-RPC client's cross-origin ``fetch``/WS to ``node_url`` is allowed. The
    raw nest URL works for native apps (no browser origin) but not for web.

    ``abort_once_paths``, when given, names stripped-of-``/app`` static-file
    paths (e.g. ``{"/fauna_wasm_bg.wasm"}``) whose FIRST GET is answered with a
    truncated body under a lying (too-large) ``Content-Length`` — a transient
    network abort the browser detects mid-transfer, not a 4xx/5xx — simulating
    a fetch cancelled by a page navigation racing an in-flight request. Every
    subsequent GET for that path serves normally, proving a retry succeeds.

    **An ``https://`` ``nest_url`` is fine, and both hops carry the same posture.**
    The HTTP hop rides the process-global ``_FloorCertHTTPSHandler`` opener
    ``tests/common/auth.py`` installs (unverified for a marked loopback nest, real
    verification for everything else), and the WS splice wraps its upstream socket
    under ``common.auth.upstream_tls_context`` — the same conjunction, so the two
    hops cannot disagree. They used to: ``_proxy_websocket`` opened a bare
    ``socket.create_connection`` and spoke **plaintext** into the TLS listener (the
    nest answered with a fatal ``decode_error`` alert, ``15 03 03 00 02 02 32``), and
    since every docker-mode nest is ``https://`` by construction, every web
    app-driven journey under ``--nest docker`` died at login in an opaque
    ``ConnectionBarrierTimeout`` with the nest's log clean.
    Pinned end-to-end by ``test_nest_mode_axis.py::
    test_the_spa_proxy_websocket_hop_speaks_tls_to_a_registered_tls_nest``.

    What this does NOT change: the two nests that exist to exercise a *client's own*
    cert-trust decision (``self_signed_nest``, ``spki_pinned_nest``) still have no
    proxy twin and their web legs stay declared absent — web makes no such decision
    (``_declare_no_web_cert_trust``), so a green leg there would witness the
    proxy's trust and not the product's. An ordinary journey against
    a TLS nest asserts nothing about trust; the proxy is the CORS shim the harness
    stands up in every mode, and the trust posture it applies is the run's own,
    the same one every native client already grants the harness-started floor
    cert.
    """
    import http.server
    import threading
    import urllib.error
    import urllib.request as urllib_req

    from common.auth import upstream_tls_context

    _aborted_once = set()

    class SPAHandler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=static_dir, **kwargs)

        def _is_api_path(self):
            # Every nest route a browser or a link holder reaches at the SPA's
            # own origin — in production the nest serves the SPA, so they are
            # one origin. `/share/` is the share-link route: web mints its links
            # against this origin (`share-links.md`), and without it the
            # stranger's fetch falls through to `index.html` below.
            return self.path.startswith(("/api/", "/admin/", "/share/"))

        def do_GET(self):
            # Proxy API requests to nest
            if self._is_api_path():
                # The web WS-RPC client opens one WebSocket per actor at
                # /api/v1/ws/{actor}; urllib can't carry a WS upgrade, so
                # splice the raw socket through instead of HTTP-proxying.
                if self.headers.get("Upgrade", "").lower() == "websocket":
                    self._proxy_websocket()
                else:
                    self._proxy_to_nest()
                return
            # Strip /app prefix for SPA file serving
            path = self.path
            if path.startswith("/app/"):
                path = path[4:]
            elif path == "/app":
                path = "/"
            if abort_once_paths and path in abort_once_paths and path not in _aborted_once:
                _aborted_once.add(path)
                # A truncated body under a Content-Length that promises more:
                # the browser detects the mismatch and rejects the fetch with a
                # network error — a real transient-abort simulation — without
                # forcibly killing the raw socket (which could also sever an
                # unrelated concurrent request sharing the same keep-alive
                # connection).
                data = (Path(static_dir) / path.lstrip("/")).read_bytes()
                self.send_response(200)
                self.send_header("Content-Type", "application/wasm")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data[: len(data) // 2])
                self.close_connection = True
                return
            file_path = Path(static_dir) / path.lstrip("/")
            if file_path.exists() and file_path.is_file():
                self.path = path
                super().do_GET()
            else:
                self.path = "/index.html"
                super().do_GET()

        def do_POST(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            self.send_error(405)

        def do_PUT(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            self.send_error(405)

        def do_DELETE(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            self.send_error(405)

        def do_PATCH(self):
            if self._is_api_path():
                self._proxy_to_nest()
                return
            self.send_error(405)

        def do_OPTIONS(self):
            """Handle CORS preflight."""
            self.send_response(200)
            self.send_header("Access-Control-Allow-Origin", "*")
            self.send_header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, PATCH, OPTIONS")
            self.send_header("Access-Control-Allow-Headers", "Authorization, Content-Type")
            self.end_headers()

        def _proxy_to_nest(self):
            """Forward request to the nest API."""
            target = nest_url + self.path
            length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(length) if length else None
            req = urllib_req.Request(
                target, data=body, method=self.command,
                headers={k: v for k, v in self.headers.items()
                         if k.lower() not in ("host",)},
            )
            try:
                resp = urllib_req.urlopen(req)
                self.send_response(resp.status)
                for k, v in resp.headers.items():
                    if k.lower() not in ("transfer-encoding",):
                        self.send_header(k, v)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()
                self.wfile.write(resp.read())
            except urllib.error.HTTPError as e:
                # Pass through the nest's HTTP error status instead of wrapping as 502
                self.send_response(e.code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()
                self.wfile.write(e.read())
            except Exception as e:
                self.send_error(502, str(e))

        def _proxy_websocket(self):
            """Splice a WebSocket upgrade through to the nest.

            Opens a raw socket to the nest, replays the handshake request
            verbatim (preserving Sec-WebSocket-Key / -Protocol so the nest's
            subprotocol-bearer validation + Accept hash work), then pumps
            bytes both ways until either side closes. This is what lets the
            web WS-RPC client's `fauna.v1, bearer.<token>` handshake reach the
            real nest in e2e (the single-threaded urllib proxy above can't).
            """
            import socket
            from urllib.parse import urlsplit

            parts = urlsplit(nest_url)
            nest_host = parts.hostname
            nest_port = parts.port or (443 if parts.scheme == "https" else 80)
            try:
                upstream = socket.create_connection((nest_host, nest_port))
                # A TLS upstream gets the run's floor-cert posture — the same
                # conjunction the HTTP hop's installed opener applies — or full
                # verification; a plain upstream stays a bare splice.
                # `ssl.SSLError` is an `OSError`, so a refused cert lands in the
                # same 502 as a refused connection, naming the cause.
                tls = upstream_tls_context(nest_url)
                if tls is not None:
                    upstream = tls.wrap_socket(upstream, server_hostname=nest_host)
            except OSError as e:
                self.send_error(502, f"ws upstream connect failed: {e}")
                return

            # Replay the handshake request line + headers, rewriting Host to
            # the nest authority. The browser sends frames only after the 101,
            # so rfile holds no buffered body to forward here.
            raw = f"{self.command} {self.path} {self.request_version}\r\n"
            for key, value in self.headers.items():
                if key.lower() == "host":
                    value = f"{nest_host}:{nest_port}"
                raw += f"{key}: {value}\r\n"
            raw += "\r\n"
            upstream.sendall(raw.encode("latin-1"))

            client = self.connection
            self.close_connection = True

            def pump(src, dst):
                try:
                    while True:
                        chunk = src.recv(65536)
                        if not chunk:
                            break
                        dst.sendall(chunk)
                except OSError:
                    pass
                finally:
                    for sock in (src, dst):
                        try:
                            sock.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass

            t_up = threading.Thread(target=pump, args=(client, upstream), daemon=True)
            t_down = threading.Thread(target=pump, args=(upstream, client), daemon=True)
            t_up.start()
            t_down.start()
            t_up.join()
            t_down.join()
            try:
                upstream.close()
            except OSError:
                pass

        def log_message(self, format, *args):
            pass

    from drivers.port_util import find_free_port
    spa_port = find_free_port()
    # ThreadingHTTPServer: a spliced WebSocket (the WS-RPC connection) stays
    # open for the whole session, so a single-threaded server would wedge —
    # every concurrent HTTP API proxy call needs its own thread.
    server = http.server.ThreadingHTTPServer(("127.0.0.1", spa_port), SPAHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return f"http://127.0.0.1:{spa_port}", server


@pytest.fixture(scope="session")
def spa_url(static_dir, nest_instance):
    """SPA proxy → the session ``nest_instance`` (the shared nest most web tests
    target). The web browser's ``node_url`` points here so its cross-origin API
    calls carry CORS headers. See ``_serve_spa_proxy``."""
    url, server = _serve_spa_proxy(static_dir, nest_instance["url"])
    yield url
    server.shutdown()


@pytest.fixture
def atproto_hosted_spa_url(static_dir, atproto_hosted_nest):
    """Function-scoped SPA proxy → the dedicated public-domain Bluesky nest, so a
    web browser in ``atproto_hosted_logged_in_app`` can reach it without CORS.
    The session ``spa_url`` only proxies ``nest_instance`` — mirrors
    ``dedicated_mail_spa_url``/``handled_spa_url``. Without this override
    ``_login_app_as``'s default ``spa_url_fixture`` points web at the SHARED
    ``nest_instance`` instead, where ``atproto_hosted_nest``'s actor is
    unregistered — every RPC then fails and the machine's snapshot never
    leaves its pre-fetch defaults (``hosted_allowed: false``), which reads
    exactly like the hosted rungs staying gated rather than an auth failure."""
    url, server = _serve_spa_proxy(static_dir, atproto_hosted_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def atproto_localhost_spa_url(static_dir, atproto_localhost_nest):
    """Function-scoped SPA proxy → the dedicated domainless Bluesky nest — same
    reasoning as ``atproto_hosted_spa_url``, for ``atproto_localhost_logged_in_app``."""
    url, server = _serve_spa_proxy(static_dir, atproto_localhost_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def rotatable_spa_url(static_dir, rotatable_nest):
    """Function-scoped SPA proxy → the dedicated ``rotatable_nest``, so a web
    browser can drive the deployment-seed rotation ceremony without CORS.

    Web's arm of the rotation-acceptance journey needs its own PLAIN-HTTP nest
    (`rotatable_tls_nest`'s real TLS binding is native-only —
    ``test_nest_rotation_admin_journey.py``'s module docstring) — mirrors
    ``atproto_hosted_spa_url``'s reasoning: the session ``spa_url`` only
    proxies ``nest_instance``, and a test that rotates its nest's identity
    must not do that to the shared session nest."""
    url, server = _serve_spa_proxy(static_dir, rotatable_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def spa_proxy_for(static_dir):
    """Factory → ``make(nest_url) -> spa_base_url``: a function-scoped SPA proxy
    over **whichever** nest this test stood up, so a web browser can reach it
    without CORS (``_serve_spa_proxy`` adds the ``Access-Control-Allow-Origin`` the
    raw nest never sends).

    The session ``spa_url`` proxies only ``nest_instance``, so every test on its own
    dedicated nest needs its own proxy — and that had been answered one named
    fixture at a time (``dedicated_mail_spa_url``, ``handled_spa_url``,
    ``rotatable_spa_url``, ``bluesky_*``, plus per-module copies in
    ``test_bridges.py`` / ``test_gated_post_compose.py``), which is why a nest with
    no such sibling — ``dedicated_caldav_only_nest``, and every
    ``unclaimed_caldav_nest`` variant — simply had no web leg available at all. One
    factory answers all of them: pass the nest URL you already hold.

    Proxies are **memoized per nest URL**: two seats on one nest (an organizer page
    and a second attendee page) share one server, exactly as
    ``alice_second_web_device`` shares ``spa_url`` with device A. Per-seat isolation
    comes from the browser context — a twin page has its own localStorage — never
    from handing each seat its own origin. Every server started is shut down at
    teardown."""
    servers = []
    by_nest_url: dict[str, str] = {}

    def make(nest_url: str) -> str:
        if nest_url not in by_nest_url:
            url, server = _serve_spa_proxy(static_dir, nest_url)
            servers.append(server)
            by_nest_url[nest_url] = url
        return by_nest_url[nest_url]

    yield make
    for server in servers:
        server.shutdown()


@pytest.fixture
def dedicated_mail_spa_url(spa_proxy_for, dedicated_mail_nest):
    """Function-scoped SPA proxy → the *dedicated* mail nest, so a web browser can
    reach it without CORS. A named alias over ``spa_proxy_for`` (one proxy
    implementation, priority #2) kept for its direct callers; a new nest wants the
    factory rather than an Nth copy of this."""
    return spa_proxy_for(dedicated_mail_nest.nest["url"])


@pytest.fixture
def handled_spa_url(static_dir, handled_nest):
    """Function-scoped SPA proxy → the *handled* nest (primary mail domain ==
    handle domain == ``fauna.test``), so a web browser in ``handled_logged_in_app``
    can reach it without CORS. The session ``spa_url`` only proxies
    ``nest_instance`` (handle-less), so the handled-actor canonical-alias tests
    need their own proxy — the web twin of pointing a native app straight at
    ``handled_nest["url"]``. Mirrors ``dedicated_mail_spa_url``."""
    url, server = _serve_spa_proxy(static_dir, handled_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def delete_account_spa_url(static_dir, delete_account_nest):
    """Function-scoped SPA proxy → the dedicated ``delete_account_nest``, so a
    web browser in ``delete_account_app`` can reach it without CORS. The
    session ``spa_url`` only proxies ``nest_instance``, which this test must
    not touch (see ``delete_account_nest``'s docstring)."""
    url, server = _serve_spa_proxy(static_dir, delete_account_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def dedicated_no_mail_spa_url(static_dir, dedicated_no_mail_nest):
    """Function-scoped SPA proxy → the *dedicated no-mail* nest, so a web browser
    in ``dedicated_no_mail_app`` can reach it without CORS. The session ``spa_url``
    only proxies ``nest_instance`` (whose actor other tests mail-enable), so the
    "actor has never held an MSEK" precondition tests need their own proxy — the
    web twin of pointing a native app straight at ``dedicated_no_mail_nest['url']``.
    Mirrors ``handled_spa_url``."""
    url, server = _serve_spa_proxy(static_dir, dedicated_no_mail_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture(scope="session")
def test_user(nest_instance):
    """Register a test user via admin API and create a default feed.

    Session-scoped: ONE identity backs `logged_in_app` and everything derived
    from it for the whole run. Accumulating state on it is the point; running
    the irreversible succession ceremony against it is fatal to every later
    test, so both it and the session nest's admin are registered as shared
    (`helpers/shared_identity.py` — what that cost on 2026-08-30 is recorded
    there, and refused at collection by
    `tests/test_no_succession_on_the_shared_identity.py`).
    """
    # A multi-tenant nest enforces the tier's quotas (the standalone binary
    # passes `enforce_tier_quotas`), and the shipped `free` seed is sized for one
    # person: 2 devices, 5 feeds. This identity accumulates both for the whole
    # run by design — `test_device_cards.py` alone registers eight devices, and
    # the feed modules create a feed per test — so it is admitted at caps that do
    # not bind. Each cap is proven nest-side (`common.auth.set_tier_caps` names
    # the conformance file for each), so this costs no coverage; a test that
    # wants to *observe* a cap sets a small one itself via `set_tier_caps`.
    #
    # Never on live. There `free` is the tier every real user of the box is on,
    # and `fauna.admin.tiers.update` has no account-scoped form; nor does the
    # harness create a tier of its own there, because `tiers.create` is global
    # too and no kind deletes a tier to reap it with. Instead the run moves its
    # OWN account onto `LIVE_HARNESS_TIER` when the box's admin has made one
    # (`users.update` on the harness's own actor, the provisioning carve-out),
    # and otherwise takes `free`'s caps as they are
    # (`nest_surface.LIVE_SELF_GATED_FIXTURES`; the shared-box rule,
    # `testing.md` § Default app and nest mode, *Live mode*).
    from common import auth
    from helpers import nest_mode as nest_mode_mod

    live = nest_mode_mod.run_mode().is_live
    admin_sk = nest_instance["admin"]["signing_key"]
    if not live:
        auth.set_tier_caps(
            nest_instance["port"],
            admin_signing_key=admin_sk,
            max_devices=auth.UNBINDING_MAX_DEVICES,
            # `max_feeds` has no nest-side ceiling to borrow, so this is simply
            # far above anything one run creates: a feed per feed-creating
            # test, with nothing deleting them.
            max_feeds=1000,
            base_url=nest_instance["url"],
        )
    user = _make_user(nest_instance)
    if live:
        from helpers.live_box_door import reaching_the_live_box

        # Session-scoped, so a box that stopped answering here would fail every
        # `logged_in_app` test's setup as a recorded `error`; the door makes it
        # the environment skip it is (`helpers/live_box_door.py`).
        with reaching_the_live_box("admitting the test user to the harness tier"):
            if auth.tier_exists(
                nest_instance["port"], admin_signing_key=admin_sk,
                name=auth.LIVE_HARNESS_TIER, base_url=nest_instance["url"],
            ):
                auth.set_user_tier(
                    nest_instance["port"], user["actor_id_hex"], auth.LIVE_HARNESS_TIER,
                    admin_signing_key=admin_sk, base_url=nest_instance["url"],
                )
    from helpers import shared_identity

    shared_identity.remember_shared_actor(user["actor_id_hex"])
    shared_identity.remember_shared_actor(
        (nest_instance.get("admin") or {}).get("actor_id_hex")
    )
    return user


@pytest.fixture(scope="session")
def api_actor_peer(nest_instance):
    """A headless second actor on the **session nest**, beside `test_user`.

    `api_actor_b`/`_c` live on `second_nest`/`third_nest` — they are the
    cross-nest actors. This one shares the driven seat's nest, which is what a
    same-nest social interaction (a like on the seat's post, a knock at its
    door) needs: those rings are produced by the nest that holds both rows, and
    a federated peer would exercise a different path entirely.

    Its own identity, never the shared one, so nothing it does accumulates on
    `test_user` (`helpers/shared_identity.py`).
    """
    from actions.api_actor import ApiActor

    peer = _make_user(nest_instance)
    return ApiActor(
        nest_url=nest_instance["url"],
        token=peer["token"],
        actor_id_hex=peer["actor_id_hex"],
        secret_bytes=bytes(peer["signing_key"]),
    )


@pytest.fixture(scope="session")
def api_actor_b(second_nest, second_user):
    """ApiActor wired to second_user on second_nest."""
    from actions.api_actor import ApiActor
    return ApiActor(
        nest_url=second_nest["url"],
        token=second_user["token"],
        actor_id_hex=second_user["actor_id_hex"],
        secret_bytes=bytes(second_user["signing_key"]),
    )


@pytest.fixture(scope="session")
def api_actor_c(third_nest, third_user):
    """ApiActor wired to third_user on third_nest."""
    from actions.api_actor import ApiActor
    return ApiActor(
        nest_url=third_nest["url"],
        token=third_user["token"],
        actor_id_hex=third_user["actor_id_hex"],
        secret_bytes=bytes(third_user["signing_key"]),
    )


_ios_setup_cache: dict | None = None
_ios_setup_done = False


def _get_ios_setup() -> dict | None:
    """Boot iOS simulator and build the app. Returns dict or None. Cached."""
    global _ios_setup_cache, _ios_setup_done
    if _ios_setup_done:
        return _ios_setup_cache
    _ios_setup_done = True

    if detect_platform() != "macos" or not shutil.which("xcodebuild"):
        return None

    # Per-session throwaway simulator → cross-working-tree iOS e2e concurrency
    # (see _create_ephemeral_simulator). Degrade to a shared pre-existing sim if
    # the runtime/devicetype probe or `simctl create` fails, so a probe gap falls
    # back to the old single-sim behaviour rather than skipping iOS entirely.
    udid = _create_ephemeral_simulator()
    if not udid:
        udid = _find_simulator_udid()
        if not udid:
            print("No iPhone simulator available (create + find both failed)")
            return None

    # Start the boot BEFORE the app build so the two overlap — a cold device
    # takes minutes to finish booting and `_build_ios_app` takes minutes to run,
    # and they need nothing from each other.
    subprocess.run(["xcrun", "simctl", "boot", udid], check=False)

    app_path = _build_ios_app()
    if not app_path:
        return None

    # …and only now collect the rest of that boot. `simctl list devices` says
    # `Booted` about a second in, while the device needs minutes more before
    # installd/FrontBoard will answer, so a launch that trusts the word runs
    # straight into an unusable device (`drivers/ios.py::_ensure_booted` carries
    # the measurement). Waiting HERE is what keeps the wait out of the first
    # test's 900 s budget: `_prebuild_binaries` calls this function at collection
    # time, and the per-launch `_ensure_booted` then finds a finished device and
    # returns at once. The build above has usually already paid for it.
    from drivers.ios import boot_and_wait_until_usable

    boot_and_wait_until_usable(udid)

    _ios_setup_cache = {"udid": udid, "app_path": app_path}
    return _ios_setup_cache


_ios_second_seat_udid_cache: str | None = None
_ios_second_seat_setup_done = False


def _get_ios_second_seat_udid() -> str | None:
    """A second, independent iOS simulator device for a CONCURRENTLY-live
    second app in the same pytest session — the one residual
    `IosInProcessDriver`'s own docstring names (`drivers/ios.py:125-127`):
    within a session every iOS launch shares `_get_ios_setup`'s single
    device, and `launch()`'s uninstall+install (:547-551) tears down whatever
    is already running there. A second-seat fixture that reused the
    organizer's device would deterministically kill the organizer's app the
    instant the second seat launches — measured as `BridgeDead`/`Connection
    refused` on the organizer's driver.

    Booted once per session (mirrors `_get_ios_setup`'s own caching) and
    shared by every second-seat fixture (`caldav_mailbox_less_attendee_app`,
    `_launch_second_real_faunamls_app`). A THIRD concurrently-live seat and
    beyond comes from `_get_ios_seat_udid`, below. `None` on any probe/create
    failure; callers skip rather than falling back to the shared device,
    since that fallback is the exact collision this function exists to avoid.
    """
    global _ios_second_seat_udid_cache, _ios_second_seat_setup_done
    if _ios_second_seat_setup_done:
        return _ios_second_seat_udid_cache
    _ios_second_seat_setup_done = True

    udid = _create_ephemeral_simulator()
    if udid:
        subprocess.run(["xcrun", "simctl", "boot", udid], check=False)
    _ios_second_seat_udid_cache = udid
    return udid


_ios_extra_seat_udids: dict[int, str | None] = {}


def _get_ios_seat_udid(ordinal: int) -> str | None:
    """The simulator device for the ``ordinal``-th CONCURRENTLY-live iOS seat of
    one test — the N-seat generalization of `_get_ios_second_seat_udid`, for a
    journey with more live members than two (the room-model journeys seat an
    owner and two members at once, `helpers/room_seats.py`).

    Seat 0 is `_get_ios_setup`'s own device and seat 1 the second-seat device,
    so a two-seat fixture and an N-seat one agree on the first two; every
    further ordinal gets its own session-scoped ephemeral device, created and
    booted on first use and reused by every later test asking for the same
    ordinal (the devices outlive a test the way the first two do; each launch's
    uninstall+install is what isolates one test's seat from the last). `None`
    on any probe/create failure — the caller skips rather than doubling up on
    a device, which is the collision this exists to avoid.
    """
    if ordinal == 0:
        setup = _get_ios_setup()
        return setup["udid"] if setup else None
    if ordinal == 1:
        return _get_ios_second_seat_udid()
    if ordinal not in _ios_extra_seat_udids:
        udid = _create_ephemeral_simulator()
        if udid:
            subprocess.run(["xcrun", "simctl", "boot", udid], check=False)
        _ios_extra_seat_udids[ordinal] = udid
    return _ios_extra_seat_udids[ordinal]


def _ensure_app_built(app_name: str) -> None:
    """Build the client's app via its just recipe (which freshens i18n + providers).

    Routes platform builds through justfile so the build-if-stale gates and
    generated-file deps fire consistently. iOS/Android only have FFI recipes
    (the iOS app is built by _build_ios_app via xcodebuild, the Android APK
    by Gradle externally), but running the FFI recipe still freshens the
    generated Swift/Kotlin sources that those builds consume.

    Memoized per pytest process (`_memoized_build`): the recipes USED TO wrap
    their cargo/MSBuild in `{{slot_build}}` ahead of any freshness check, so
    re-running `just` per driver-config build charged a slot wait to whichever
    test triggered it, even on a fully warm tree. Every app recipe now gates
    freshness outside the slot (a warm tree takes none), and the memo spares
    the gate's own source scan per call. Normally it is already warm from
    `_prebuild_binaries` at collection time, outside every per-test budget.
    """
    _memoized_build(
        f"app:{app_name}", lambda: _build_app_via_just(app_name)
    )


def _restore_full_apple_ffi() -> None:
    """Re-assemble the FULL (multi-slice) `FaunaFFI.xcframework`.

    Runs at collection time AFTER every app build, whenever a run selects both
    `ios` and `macos`. `_prebuild_binaries` builds apps in `sorted(apps)` order,
    so `app[ios]` (`just apple-ffi-test`, writes the full slice set) always precedes
    `app[macos]` (`just mac-debug` → `just apple-ffi-host-test`, which `rm -rf`s that
    framework and writes the 1 darwin slice — see `_xcframework_slices`). The
    iOS app is then built lazily at the first iOS test and cannot link, so
    `--app macos,ios` lost its **entire iOS leg** to a silent skip; measured
    2026-07-30 on macOS, 15/15 iOS tests SKIPPED in `test_events.py`.

    Restoring here rather than at iOS-build time is deliberate: `just` recipes
    take the machine-wide build slot unconditionally, and this function's whole
    reason for existing is to keep that wait out of a test's `timeout = 900`
    (see `_prebuild_binaries`). The macOS app is already linked by now, so
    swapping the framework back cannot unbuild it, and the restore is a
    cross-checkout cache hit on a warm tree.
    """
    _build_via_just("apple-ffi-test", "restoring the full FaunaFFI.xcframework")


def _ensure_macos_pkg_built() -> None:
    """Build (or reuse the cached) macOS `.pkg` installer at COLLECTION time —
    outside every per-test pytest-timeout budget.

    `tests/platform/macos/test_installer.py`'s `pkg_path` fixture (`TestDryRun`
    / `TestFullInstall`) used to build the `.pkg` inside the FIRST requesting
    test's own setup, subject to `timeout = 900`. `installer/macos/build.sh`
    (`just pkg-unsigned`) takes the machine-wide `build` slot unconditionally,
    and under fleet contention the queue alone can run well past 900s
    (measured 2026-08-21/22: >29 min queued behind disk-floor churn before the
    ~12 min cold build even started) — so a perfectly healthy build died as a
    bare `Timeout (>900.0s)`, the exact bound inversion this function's
    siblings (mail/atproto bridges, seal-helper, sync, the client apps) were
    each hoisted here to prevent. `pkg_path` was the one builder that had
    never been hoisted.

    Guarded by `sys.platform` rather than relying on `pytest_collection_modifyitems`
    deselection: `test_installer.py` gates itself with a bare
    `pytest.mark.skipif(sys.platform != "darwin")`, not an `--app`/marker-based
    platform tag, so its items (and `pkg_path` in their fixture closure) are
    still COLLECTED — just skipped at run time — on non-macOS machines. Without
    this guard, a Linux or Windows run would try to build a macOS `.pkg` at
    collection time on every invocation that collects this file.

    The actual build/freshness-cache logic (mtime + stamp cache, the
    root-build refusal) stays in `test_installer.py` — `_ensure_pkg_built`
    there is the single function both this hoist and the real `pkg_path`
    fixture call, memoized (`_memoized_build`) so only one of the two doors
    ever does the real work.
    """
    if sys.platform != "darwin":
        return
    from tests.platform.macos.test_installer import _ensure_pkg_built

    _ensure_pkg_built()


#: Set once at `pytest_configure` from `--macos-artifact`. Run-level and
#: single-valued: a run drives EITHER the bare binary or the `.app` bundle.
_MACOS_ARTIFACT_MODE = False


def _build_app_via_just(app_name: str) -> None:
    recipes = {
        "macos": "mac-app debug" if _MACOS_ARTIFACT_MODE else "mac-debug",
        "linux": "linux-debug",
        "windows": "windows-debug",
        # The TEST FFI flavor: the iOS simulator app is built Debug, so its
        # `#if DEBUG` FaunaKit TestAgent — the thing this harness drives — calls
        # the `*ForTest` UniFFI seams, which only the `test-helpers` bindings
        # carry. `just apple-ffi` is the PRODUCTION flavor and would not link
        # (testing.md § convention 15).
        "ios": "apple-ffi-test",
        "android": "android-debug",
        "tui": "tui-debug",
    }
    recipe = recipes.get(app_name)
    if recipe:
        _build_via_just(recipe, f"building the {app_name!r} app")


# Ported from the fleet's own merge-gate infra-failure classifier (kept
# byte-identical across all three machine copies) — the same disk/build-slot
# exhaustion signature, reused here so a prebuild failure gets the same
# classification a merge-gate check already ratified. Anchored to the infra
# tool's own voice:
# each alternative must START the line, either `[build-slot]` (build-slot's
# own prefix) or `error` (rustc/cargo's own top-level fatal-error convention).
# An unanchored search over build output scored a genuine code red as INFRA
# whenever the output merely QUOTED one of these substrings (a rustc
# source-quote excerpt; a dep's `cargo:warning=` text) — neither of which ever
# starts a line with `error` or `[build-slot]`. the disk-quota case surfaces here as a bare
# `building the '<app>' app FAILED (exit 101)` with the real cause buried deep
# in the tail, and the harness's own retry-failed-twice advice
# ("suspect a real break") is exactly backwards for it.
_INFRA_RE = re.compile(
    r"^(\[build-slot\]|error).*(Disk quota exceeded|No space left on device|"
    r"DISK FLOOR:|slot freed within|cross-pool order violation)"
)


def _strip_drain_pipes_tag(line: str) -> str:
    """Undo `drivers.port_util.drain_pipes`' `[out] `/`[err] ` prefix.

    `_INFRA_RE` is anchored to the START of the subprocess's own output line
    (`error`/`[build-slot]`) — exactly what the ratified shell-script version
    reads from a raw log file. `drain_pipes` instead stores each line as
    `f"[{tag}] {line}"`, so matching the stored line directly would never hit
    the anchor; strip the tag back off first.
    """
    for tag in ("[out] ", "[err] "):
        if line.startswith(tag):
            return line[len(tag):]
    return line


def _build_via_just(recipe: str, what: str) -> None:
    """Run `just <recipe>`, streaming its output, and raise with a bounded tail
    on failure. `what` names the caller's intent for the error message.

    Retries the recipe ONCE on failure before raising, EXCEPT when the failure
    is a `{{slot_build}}` wait timeout (2026-08-06 — follow-up (b) of the
    announce-build trap, RECONCILED after two sessions ruled it independently
    and collided: a NO-retry ruling landed first, reasoning that
    `{{slot_build}}` already bounds contention via `FAUNA_SLOT_TIMEOUT` (5400s
    for the build pool), so any failure reaching this line is either that wait
    timing out (retrying queues another 90 minutes) or a genuine build break
    (retrying just doubles the wall clock). That dichotomy misses the failure
    this item was filed from: observed live 2026-07-29, a seat build broke
    with a bare non-zero exit while a DIFFERENT concurrent build held the
    OTHER build slot compiling `fauna-ffi` — this seat had already ACQUIRED
    its own slot and broke mid-compile, nothing like a 90-minute wait;
    re-running by hand recovered clean in seconds. `build-slot.py` bounds the
    *count* of concurrent slot-holders, not resource contention *between*
    them (its own docstring: "NO nice and NO affinity mask"). So: retry once,
    but detect and skip the genuine slot-wait-timeout case specifically (by
    `build-slot.py`'s own exact `TimeoutError` phrase, "slot freed within") —
    retrying THAT would turn one bounded failure into two, exactly the risk
    the first ruling correctly flagged. Every other failure, including the
    actual 2026-07-29 signature, is retried once; a genuine compile error
    fails the same way both times and still raises, with both attempts'
    tails quoted so they can be compared.
    """
    # Announce WHAT is being built before building it. Without this line a
    # warm build is textually invisible, so a log reader cannot tell
    # "the build gate ran and had nothing to do" from "the gate never
    # fired" — and on the multiseat announce path those two readings differ
    # by a whole aborted cohort (2026-07-24: an id announced over a seat
    # that was still compiling stranded the other two machines, which had
    # already been told to start). Cost a session ~15 min of doubting a
    # sound `_ensure_local_seat_built` on 2026-07-29 before build-slot
    # occupancy settled it; the gate was fine, only its silence was.
    #
    # TEE, not capture (convention 13's drain contract + convention 6's
    # self-diagnosis). Both naive options are wrong here: capturing makes a
    # multi-minute build look HUNG because nothing is emitted until it exits,
    # while letting the recipe stream to an inherited fd loses the output on
    # the paths where it never reaches the pytest log — which is how an
    # announce-time seat-build break arrived as a bare CalledProcessError
    # with ZERO build output (2026-07-29), i.e. the cohort-stranding case
    # with no evidence. So stream live AND keep a bounded tail to quote back.
    from drivers.port_util import (
        drain_pipes,
        popen_group_kwargs,
        reap_descendants_of,
        wait_pipes_drained,
    )

    # Resolved via `shutil.which`, not left as a bare "just" for `Popen`/
    # `CreateProcess` to search PATH for itself — on Windows, `CreateProcess`'s
    # own bare-name PATH search only ever appends `.exe`, never consulting
    # PATHEXT, so it cannot find a `.cmd` shim (measured: a `.cmd` fake on a
    # replaced PATH raises `FileNotFoundError [WinError 2]` even though it's
    # right there). `shutil.which` DOES honour PATHEXT, and passing its
    # resolved path (extension included) straight to `Popen` runs a `.cmd`
    # directly with no `shell=True` (measured). Failing loudly here, instead
    # of via a bare `Popen(["just", ...])` raising deep inside stdlib, is
    # itself convention 6 (a failure must diagnose itself).
    just_bin = shutil.which("just")
    if not just_bin:
        raise RuntimeError(f"{what} failed: no `just` found on PATH")

    failed_attempts = []
    skip_reason = None
    for attempt in (1, 2):
        label = "" if attempt == 1 else " (retry)"
        print(f"[build] {what} -> just {recipe}{label}", flush=True)
        # Armed against point 9 even though this spawn is synchronous (`proc.wait()`
        # below). The guarantee is not about the happy path: `just` fans out to
        # cargo/rustc, so a pytest that is SIGKILLed or reaped mid-build — a routine
        # event here, where the harness can reap a backgrounded task and builds queue
        # behind a machine-wide slot — orphans that whole tree. The orphan keeps
        # compiling, holds its build slot against the next session, and holds its
        # artifact against the next compile: exactly the cost `reap_descendants_of`
        # documents. Unarmed until 2026-08-14, and invisible to the structural pin,
        # which passed this file on its three OTHER armed spawns.
        proc = subprocess.Popen(
            # Split so a recipe may carry arguments (`mac-app debug` — the artifact
            # mode's build). Every argument-less recipe splits to itself.
            [just_bin, *shlex.split(recipe)],
            cwd=_repo_root,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            **popen_group_kwargs(),
        )
        reap_descendants_of(proc.pid)
        recent = drain_pipes(proc, maxlen=400, echo=sys.stderr)
        rc = proc.wait()
        if rc == 0:
            if attempt == 2:
                print(
                    f"[build] {what} succeeded on retry — the first failure "
                    f"was transient (likely build-slot contention with "
                    f"another concurrent build), not a real break",
                    flush=True,
                )
            return
        # Block until the reader thread has actually reached EOF, not just
        # until `recent` holds *a* line. `recent` can go non-empty after the
        # thread's first `readline()` (e.g. a "Compiling..." announce line)
        # while the next call — carrying the real error — hasn't returned
        # yet; under load that gap is wide enough that quoting on "recent is
        # truthy" genuinely truncates the tail to just the announce line
        # (testing.md convention 14: this was itself a convention-13
        # violation one level up — output that existed but wasn't drained by
        # the time it was read).
        wait_pipes_drained(proc, timeout=10.0)
        failed_attempts.append((rc, list(recent)))
        if attempt == 1:
            # A `{{slot_build}}` WAIT timeout must NOT be retried: it means
            # this recipe already waited up to FAUNA_SLOT_TIMEOUT (5400s) for
            # a free slot and never got one, so retrying queues ANOTHER
            # up-to-90-minute wait — turning one bounded failure into two, by
            # far the worst outcome for an announce-time cohort.
            # The build-slot tool raises `TimeoutError` with this exact
            # phrase when that happens.
            if any("slot freed within" in line for line in recent):
                skip_reason = (
                    "the failure was a build-slot WAIT timeout, not a compile "
                    "break — retrying would only queue another long wait"
                )
                print(
                    f"[build] {what} FAILED (exit {rc}) — {skip_reason}, so "
                    f"NOT retrying",
                    flush=True,
                )
                break
            print(
                f"[build] {what} FAILED (exit {rc}) — retrying once before "
                f"giving up (a lone failure is often transient build-slot "
                f"contention with another concurrent build)",
                flush=True,
            )

    sections = []
    for i, (rc, recent) in enumerate(failed_attempts, start=1):
        tail = "\n".join(f"    {line}" for line in recent[-60:])
        sections.append(
            f"--- attempt {i}: `just {recipe}` exited {rc}, last "
            f"{min(len(recent), 60)} line(s) ---\n"
            f"{tail or '    <no output captured>'}"
        )
    if skip_reason:
        summary = f"{what} failed and was NOT retried: {skip_reason}.\n"
        repro_note = (
            f"transient build-slot contention is unlikely here — this was a "
            f"WAIT timeout, so check who is holding both machine-wide build "
            f"slots."
        )
    else:
        summary = f"{what} failed on BOTH the original attempt and the retry.\n"
        repro_note = (
            f"Failing identically twice makes transient build-slot contention "
            f"unlikely — suspect a real break. If the two attempts' output "
            f"differs, check the machine-wide build-slot status."
        )
    # Classify BEFORE the generic "suspect a real break" advice reaches the
    # reader — a disk/build-slot exhaustion signature means the two attempts
    # failed identically because the cause is persistent, not because it is a
    # compile error. Checked across every
    # attempt's tail, not just the last, so it fires even when only the FIRST
    # attempt logged the signature.
    infra_hit = next(
        (
            line
            for _, recent in failed_attempts
            for line in recent
            if _INFRA_RE.match(_strip_drain_pipes_tag(line))
        ),
        None,
    )
    if infra_hit:
        summary = (
            f"⚠ INFRA, not a code break — the build output matched a known "
            f"disk/build-slot exhaustion signature: {infra_hit!r}\n"
            f"The cheap, safe reclaim is `rm -rf <target>/debug/incremental` "
            f"(pure rebuild-speed cache — no built artifact lost); "
            f"`cache-release` does NOT free anything now, only stamps "
            f"the dataset cleanable for the machine sweeps. If the signature "
            f"names a slot problem instead, check the machine-wide build-slot "
            f"status.\n\n"
        ) + summary
    raise RuntimeError(
        summary
        + "\n".join(sections)
        + "\n--- end build output ---\n"
        f"Reproduce with:\n"
        f"    just {recipe}\n"
        f"(run it from {_repo_root}). {repro_note}"
    )


def _installed_windows_app_override(request) -> str | None:
    """Session-wide: if any selected test carries the `installed_product` marker,
    point the windows driver at the REAL MSI-installed app
    (``%ProgramFiles%\\Fauna\\App\\FaunaApp.exe``) instead of the dev build.

    That is the whole point of the installer full-journey test: the dev build
    spawns its sync agent out of the build tree, while the installed app resolves
    ``baseDir\\..\\fauna-sync-agent.exe`` (``App.xaml.cs`` ``SpawnSyncAgentDetached``) and so
    exercises the MSI's *own* agent — the installer→agent→provisioning chain that
    broke live on 2026-07-17 and that no test crossed before.

    Session-wide (like :func:`_apply_real_conversations_env`) because the native
    app is session-scoped via ``_driver_cache``; run the installer journey in its
    own pytest invocation so this never reaches the dev-build windows tests.

    **Raises rather than falling back** when the marker is set but the product is
    absent: silently reverting to the dev build would green a test whose entire
    subject is the installed binaries — the most expensive kind of false pass.
    The MSI-installing fixture must be ordered before the app fixture.
    """
    if not any(
        item.get_closest_marker("installed_product") for item in request.session.items
    ):
        return None
    installed = (
        Path(os.environ.get("ProgramFiles", r"C:\Program Files"))
        / "Fauna"
        / "App"
        / "FaunaApp.exe"
    )
    if not installed.exists():
        raise RuntimeError(
            f"`installed_product` marker is active but {installed} does not exist — "
            "the MSI must be installed before the app fixture launches. Order the "
            "installing fixture ahead of `app`/`logged_in_app` in the test signature."
        )
    return str(installed)


def _apply_installed_product_agent_env(env: dict, request) -> None:
    """Session-wide: an `installed_product` session launches the windows app with
    the agent pin explicitly EMPTY, the contract's own "not pinned" (Rust
    ``pinned_agent_binary``; C# ``E2eEnv.SyncAgentBin`` read via ``IsNullOrEmpty``).

    The installed app is built with the test-flavored FFI (the journey drives it
    through the e2e agent), so it honours ``FAUNA_E2E_SYNC_AGENT_BIN``, and the
    windows driver defaults that pin to this checkout's
    ``target\\debug\\fauna-sync-agent.exe`` on every launch. Left alone, the
    installed app spawned the DEV agent and the journey's whole subject, "the
    installed app brings up the installed agent", was swapped out by the harness
    (measured 2026-09-27, the first elevated run with a buildable MSI). The driver's default is a ``setdefault``
    (``drivers/windows.py::_default_sync_agent_pin``), so the explicit empty
    value survives it.
    """
    if any(
        item.get_closest_marker("installed_product") for item in request.session.items
    ):
        env["FAUNA_E2E_SYNC_AGENT_BIN"] = ""


def _installed_linux_app_override(request) -> str | None:
    """Session-wide: if any selected test carries the `installed_product` marker,
    point the linux driver at the app ``install.sh`` INSTALLED rather than the
    binary sitting in the build tree.

    The linux half of the same idea as :func:`_installed_windows_app_override`,
    and it buys the same thing: the dev build finds its sync agent next to itself
    in ``target/debug``, while the installed app resolves it beside the installed
    binary (``sync_agent.rs`` ``agent_binary_absolute``) — so the
    install-channel→agent→launch chain has a witness instead of a file-layout
    assertion. ``helpers/linux_installed_product`` owns the prefix, the staging
    seam and the reason the staged binaries are debug ones.

    **Raises rather than falling back** when the marker is set but nothing is
    installed — the windows override's rule, for the windows override's reason:
    quietly reverting to the build-tree binary would green a test whose entire
    subject is the installed one. The installing fixture must be ordered before
    the app fixture.

    Session-wide (like :func:`_apply_real_conversations_env`) because the native
    app is session-scoped via ``_driver_cache``; run the installed journey in its
    own pytest invocation so this never reaches the ordinary linux tests.
    """
    if not any(
        item.get_closest_marker("installed_product") for item in request.session.items
    ):
        return None
    from helpers.linux_installed_product import installed_app_path

    installed = installed_app_path(_repo_root)
    if not installed.exists():
        raise RuntimeError(
            f"`installed_product` marker is active but {installed} does not exist — "
            "`install.sh` must have run before the app fixture launches. Order the "
            "installing fixture ahead of `app`/`logged_in_app` in the test signature."
        )
    return str(installed)


def _installed_tui_app_override(request) -> str | None:
    """Session-wide: if any selected test carries the `installed_product` marker,
    point the tui driver at the `fauna-tui` the ARCHIVE's `install.sh` installed
    rather than the binary sitting in the build tree.

    The terminal app's statement of the same idea as the linux and windows
    overrides: its channel is the per-OS archive (`installers/tui.md` § The
    ratified channel), whose `install.sh` copies `fauna-tui` and
    `fauna-sync-agent` into a prefix — and the app resolves the agent beside
    itself (`agent_spawner::agent_binary_absolute`), so the channel dropping it
    is exactly the class of bug only an install-then-drive test sees.
    ``helpers/tui_installed_product`` owns the prefix, the staging seam (the
    real `assemble-archive.sh` over the DEBUG binaries) and the isolation.

    **Raises rather than falling back** when the marker is set but nothing is
    installed — the sibling overrides' rule, for their reason. Session-wide
    (`_driver_cache`); run the installed journey in its own pytest invocation.
    """
    if not any(
        item.get_closest_marker("installed_product") for item in request.session.items
    ):
        return None
    from helpers.tui_installed_product import installed_app_path

    installed = installed_app_path(_repo_root)
    if not installed.exists():
        raise RuntimeError(
            f"`installed_product` marker is active but {installed} does not exist — "
            "the archive's `install.sh` must have run before the app fixture launches. "
            "Order the installing fixture ahead of `app`/`logged_in_app` in the test "
            "signature."
        )
    return str(installed)


def _apply_r14_trust_env(
    env: dict, nest_instance: dict, request, *, build_commit: str | None = None
) -> None:
    """Seed the R14 (account-data-plane.md § The ratified decisions) escrow-holder trust so the generation plane runs LIVE in e2e.

    Default-ON for every app launch (`e2e-automation-surface-gating.md` § The
    e2e trust seed owns the contract). A plaintext e2e nest can never graduate
    a TLS channel-binding pin, so `trust::trusted_escrow_holders` returned an
    empty set in every app-e2e run ever made: no tip resolved, every fleet-only
    sealed write refused honestly, `device_endpoints: Unmintable` in every pump
    report — the whole R14 plane dormant and nobody noticing, because refusal
    IS the fail-safe design (found 2026-08-17, while building the plane's first
    app-level consumer: `tests/test_custody_ceremony_journey.py`).

    The seed is the e2e's stand-in for that pin: nest.info's `nest_id` (the
    identity a client WOULD pin), read by `trusted_escrow_holders` in
    test-capable builds only — an inner runtime switch within a test-capable
    build, never the security boundary (convention 15). It must be in the
    launch environment BEFORE the app starts, because the account runtime
    captures its trust set at assembly; `config["environment"]` is exactly
    that, and it beats a process-global `os.environ` write: scoped to the app
    (and the sync agent it spawns as a child), never to the harness's other
    subprocesses.

    **Keyed by the nest it names.** The pin is per nest authority, so the seed
    is too: `<nest url>=<nest_id>` entries (`_merge_trust_seed`). Calling this
    again for ANOTHER nest into the same environment adds that nest's entry
    beside the first, which is what a launch that will talk to a second nest
    needs. The unkeyed seed trusted the launch nest's key for every nest, so an
    app pointed at a dedicated nest ran each generation mint there to a
    post-deposit refusal ("signed by a holder this account does not trust").
    An environment fixed at launch can only name the nests that exist at
    launch, so an app is LAUNCHED against the nest it will use, seeded for it —
    never re-pointed at a nest its launch did not name.

    **A previous release's binary is seeded in the grammar IT reads.** A build
    older than the keyed grammar (`helpers.prev_build.KEYED_TRUST_SEED_COMMIT`)
    reads the variable as one bare identity and ignores a keyed entry, while
    today's reader honours a bare identity for no nest. So a launch of a binary
    built from ``build_commit`` (the version-skew grid's pinned release) gets
    the bare form exactly when that commit predates the keyed grammar, decided
    by ancestry, and the bare arm retires by itself once the pin moves past it.
    A launch of the working tree's own build passes no ``build_commit`` and is
    always keyed.

    No app is a declared absence any more. **web** WAS one — it runs in wasm,
    where `std::env::var` has nothing to read — until the SPA began hosting the
    account runtime in-tab: its launch writes the same keyed entries into the
    origin's `localStorage` (`drivers/web.py::launch`), and the wasm side reads
    them in the one grammar `fauna_client_core::nest_trust::seeded_nest_identity`
    shares with native.

    **android** WAS a declared absence for the same structural reason (Intent
    launch, not exec — only keys `BridgeHttpServer.kt::launchApp()` explicitly
    forwards ever arrive) until recently: `launchApp()` now
    forwards this one key as an intent extra, and `MainActivity.onCreate`
    re-exports it into the process environment via `android.system.Os.setenv`
    before any auto-login can assemble the account runtime — the runtime reads
    it via the same `std::env::var` door every other app uses.

    Opt-out: the `no_r14_trust` marker, session-wide like
    `_apply_real_conversations_env` — the app drivers are session-scoped
    (`_driver_cache`), so a per-test opt-out could not un-seed an already
    launched app. A test asserting the no-trust fail-safe posture therefore
    carries the marker AND runs in its own invocation.

    Skipped for the `live` nest mode: a real TLS nest graduates a real pin,
    which is the production path this seed stands in for (true of every TLS
    nest only since 2026-10-05 — before the login's-pin ruling, `security.md`
    § Transport trust, a public-CA nest that no claim had seeded pinned
    nothing, which is exactly what the first live tui sweep exposed: every
    GenerationTip write refused as "a holder this account does not trust";
    live mode stays unseeded on purpose, so that it keeps testing the real
    pin) — and `nest.info`
    is unreachable anyway, since `_LiveProvider`'s declared `port` (443, the
    box's own HTTPS port) is not a `127.0.0.1` port `ws_api.nest_info` could
    dial (found 2026-08-27, `e2e-live-mode-test` cascading `ConnectionRefusedError`
    on every app-fixture test since `_apply_r14_trust_env` went default-on
    2026-08-17 — no live sweep had run against it since). The stale guard this
    replaces (`if not port: return`) assumed a live handle carries no `port`,
    which stopped being true 2026-08-02 when `_LiveProvider` started declaring
    one (`nest_mode.md`/`testing.md` capability-honesty contract).
    """
    if any(
        item.get_closest_marker("no_r14_trust")
        for item in request.session.items
    ):
        return
    from helpers import nest_mode as nest_mode_mod

    if nest_mode_mod.run_mode().is_live:
        return
    port = nest_instance.get("port")
    if not port:
        return
    nest_url = nest_instance.get("url")
    if not nest_url:
        raise AssertionError(
            f"nest handle {nest_instance!r} carries a port but no url — the R14 "
            "trust seed is keyed by the nest it names, so it cannot be built "
            "(see _apply_r14_trust_env)"
        )
    hex_id = _nest_id_hex(port)
    if build_commit is not None:
        from helpers.prev_build import reads_keyed_trust_seed

        if not reads_keyed_trust_seed(build_commit):
            env["FAUNA_E2E_TRUST_NEST_IDENTITY"] = hex_id
            return
    env["FAUNA_E2E_TRUST_NEST_IDENTITY"] = _merge_trust_seed(
        env.get("FAUNA_E2E_TRUST_NEST_IDENTITY", ""), nest_url, hex_id
    )


def _seeded_environment(request, *nests: dict) -> dict:
    """A launch environment carrying the escrow-trust seed for each of ``nests``.

    The inline form of :func:`_apply_r14_trust_env` for an app a test launches
    itself: ``"environment": _seeded_environment(request, nest_instance)``, or
    spread first into an environment of its own
    (``{**_seeded_environment(request, nest), "FAUNA_BOUND_ACCOUNT": actor}``).
    Name every nest the launched app will use; the environment is fixed at
    launch. `tests/test_r14_trust_seed_self_launch.py` fails on a self-launch
    that reaches neither this nor the writer.
    """
    environment: dict = {}
    for nest in nests:
        _apply_r14_trust_env(environment, nest, request)
    return environment


def _trust_seeder(request):
    """:func:`_apply_r14_trust_env` bound to ``request``, called as
    ``seed(environment, nest)`` — the writer a launcher outside conftest takes as
    an argument, since a shared module does not import conftest
    (``make_launch_harness(..., seed_trust=_trust_seeder(request))``)."""
    return lambda environment, nest: _apply_r14_trust_env(environment, nest, request)


def _nest_id_hex(port) -> str:
    """nest.info's `nest_id` for the nest on ``port``, as 64 hex — the identity a
    client WOULD pin, and so the one the trust seed names.

    Read per call, never cached: the nest identity is exactly the thing a
    rotation test changes mid-session (`test_nest_rotation_*`), and a port-keyed
    cache would hand a later launch the identity of a nest that no longer
    exists. One WS round trip per launch is nothing beside a build.

    nest.info's `nest_id` IS the nest identity clients pin
    (`discovery_core::nest_info_core`: `state.nest_identity.public_key_bytes()`).
    """
    from tests.api import ws_api

    raw = ws_api.nest_info(port).get("nest_id")
    if isinstance(raw, (bytes, bytearray, list)):
        return bytes(raw).hex()
    if isinstance(raw, str) and len(raw.strip()) == 64:
        return raw.strip()
    raise AssertionError(
        f"nest.info carries no usable nest_id: {raw!r} — the R14 trust "
        "seed cannot be built, so every fleet-only sealed write in this "
        "run would refuse (see _apply_r14_trust_env)"
    )


def _merge_trust_seed(seed: str, nest_url: str, hex_id: str) -> str:
    """``seed`` with ``nest_url``'s entry set to ``hex_id``, one entry per nest.

    The grammar is the one door's (`trust::trusted_escrow_holders`, pinned by
    `libs/fauna-anon-client/tests/e2e_trust_seed.rs`): ``<nest url>=<64 hex>``
    entries joined by ``,``, each key normalized there with ``authority_of``. A
    fresher read of a nest replaces its own entry rather than sitting beside a
    stale one; every other nest's entry is kept.
    """
    entries = [
        entry
        for entry in seed.split(",")
        if entry and entry.rsplit("=", 1)[0] != nest_url
    ]
    entries.append(f"{nest_url}={hex_id}")
    return ",".join(entries)


def _trust_seed_urls(seed: str) -> set[str]:
    """The nest urls a trust seed names (`_merge_trust_seed`'s grammar)."""
    return {entry.rsplit("=", 1)[0] for entry in seed.split(",") if "=" in entry}


def _relaunch_trusting_nest(driver, nest_instance: dict) -> None:
    """Relaunch ``driver``'s app seeded for ``nest_instance`` if its launch did not
    name that nest — the call a login seam makes BEFORE it points a
    session-cached app at a nest (`e2e-automation-surface-gating.md` § The e2e
    trust seed).

    An app's escrow trust is captured when its account runtime assembles, from
    an environment fixed at launch, so a nest the launch did not name can be
    trusted only by a launch that names it. The relaunched seed is the one the
    app LAUNCHED with plus this nest's entry — rebuilt from the launch value on
    every call, never accumulated: a dedicated nest is gone when its test ends,
    and the next one may take its port.

    No relaunch, and no dial, when:

    * the nest is one the app launched against — trusted as launched;
    * the launch environment carries no seed at all — the session is
      deliberately unseeded (`no_r14_trust`, the `live` nest mode), and a
      re-point must not seed it;
    * the driver has no relaunchable environment
      (`HttpBridgeDriver.relaunch_environment` is None): web reads none, and an
      app that cannot cold-relaunch keeps that gap, declared where it matters;
    * the handle is port-less (a live stub).

    No relaunch either when the app already carries this nest's CURRENT identity
    (a second login on the same dedicated nest); a new nest on a recycled port
    reads differently and does relaunch. A relaunch is a fresh launch
    (convention 10), which is why a seam calls this before its ``set_state`` and
    never after.

    Before any of that, a DEVICE app (android) is given the nest's port: the
    device reaches a nest only as its own loopback through an ``adb reverse``
    (`testing.md` § Default app and nest mode → *Android's run venue*,
    constraint 3), and a nest started after the launch was never reversed.
    Every re-point seam calls this function, so it is the one place that opens
    it — on every call, relaunch or not (``ensure_reverse`` is idempotent).
    """
    reach_port = nest_instance.get("port")
    ensure_reverse = getattr(driver, "ensure_reverse", None)
    if ensure_reverse is not None and reach_port:
        ensure_reverse(reach_port)
    relaunch_environment = getattr(driver, "relaunch_environment", None)
    environment = relaunch_environment() if relaunch_environment else None
    if environment is None:
        return
    seed = environment.get("FAUNA_E2E_TRUST_NEST_IDENTITY")
    if not seed:
        return
    port, nest_url = nest_instance.get("port"), nest_instance.get("url")
    if not port or not nest_url:
        return
    launched = getattr(driver, "_e2e_launched_trust_seed", None)
    if launched is None:
        launched = driver._e2e_launched_trust_seed = seed
    if nest_url in _trust_seed_urls(launched):
        return
    hex_id = _nest_id_hex(port)
    if f"{nest_url}={hex_id}" in seed.split(","):
        return
    environment["FAUNA_E2E_TRUST_NEST_IDENTITY"] = _merge_trust_seed(
        launched, nest_url, hex_id
    )
    if not driver.recover():
        raise AssertionError(
            f"could not relaunch the app seeded for {nest_url} — without that "
            "relaunch its account runtime trusts no escrow holder there, and "
            "every generation mint on that nest is refused (see "
            "_relaunch_trusting_nest)"
        )
    driver.wait_for_state(lambda state: state is not None, timeout=30)


@contextlib.contextmanager
def _web_onboarding_routed_to(driver, spa_url: str):
    """Route a WEB app's onboarding — its pre-identity calls AND the session it
    concludes into — at ``spa_url`` for the ``with`` block; a no-op elsewhere.

    Web's twin of ``_relaunch_trusting_nest`` for an onboarding journey that
    joins, claims or signs in on a nest the page was not served by. Web is
    single-origin: until a ``LoggedIn`` terminal records a nest, every dial it
    makes resolves to the page's own origin (``storedNestUrl()`` in
    ``apps/fauna-web/src/lib/api.ts``) — the SESSION nest — so a join on a
    dedicated nest registers there and then authenticates against a box that
    has never heard of the actor. The ``nest`` provider override is the
    harness seam onboarding.md § 2 (*Local / self-hosted targets*) names for
    this: the page installs it as both the onboarding-provider override and the
    nest dial override, so every call reaches ``spa_url`` — a SPA proxy onto
    the dedicated nest, same-origin in all but port and CORS-headed.

    Cleared on the way out however the block exits: the override rides the
    page's query string across every later reload, so leaking it would point
    the next test's onboarding at a nest that is gone by then.
    """
    if not driver.is_web():
        yield
        return
    driver.set_provider_base_urls({"nest": spa_url})
    try:
        yield
    finally:
        # An empty map is the clear: `provider_base_url("nest")` finds no entry.
        try:
            driver.set_provider_base_urls({})
        except Exception:
            pass


def _apply_release_feed_env(env: dict, request) -> None:
    """Point the app's newer-version check at the harness's stub release feed.

    `installers/README.md` § Knowing a newer version is out is a promise about
    a feed the harness cannot control — GitHub's `releases/latest` for
    `RELEASE_REPO` — so a walk of it serves the endpoint itself
    (`helpers/release_feed_stub.py`) and hands the app its origin through the
    compile-gated `FAUNA_E2E_RELEASE_FEED_URL` seam. Only the origin moves: the
    path, the JSON shape and the semver decision stay the shared production
    ones (`fauna_core::version`).

    **Marker-gated** (`release_feed`), for the mail-import seed's reason: the
    stub is meaningful only to a test that walks the check. A session with no
    such test still never reaches the production feed: since the
    once-per-sign-in look (§ Knowing a newer version is out, amended
    2026-10-03), every sign-in reads the feed, so the origin is a closed
    loopback port there (`UNREACHABLE_FEED_ORIGIN`) and the look fails silently,
    as a failed look must.

    Session-wide, like every launch-environment seed: the native app drivers
    are session-scoped (`_driver_cache`), so the origin must be in the launch
    environment before the first launch. The stub itself is the session
    fixture `release_feed_stub`, started here on demand.

    Who reads it today: **tui** (`settings/about.rs::feed_origin`). The linux,
    windows and macOS checks are built but read no seam yet; seeding them is harmless, so the
    helper is called from every native branch rather than special-cased.
    """
    if not any(
        item.get_closest_marker("release_feed") for item in request.session.items
    ):
        env["FAUNA_E2E_RELEASE_FEED_URL"] = UNREACHABLE_FEED_ORIGIN
        return
    env["FAUNA_E2E_RELEASE_FEED_URL"] = request.getfixturevalue("release_feed_stub").origin


#: The feed origin of a session that walks no update check: the discard port on
#: loopback, closed on every dev machine, so a sign-in look is refused at once
#: and never leaves the box.
UNREACHABLE_FEED_ORIGIN = "http://127.0.0.1:9"


@pytest.fixture(scope="session")
def release_feed_stub():
    """The stub release feed (`helpers/release_feed_stub.py`), for the session.

    Requested by `_apply_release_feed_env` when a selected test carries the
    `release_feed` marker, and by that test to read what the app requested.
    """
    from helpers.release_feed_stub import ReleaseFeedStub

    stub = ReleaseFeedStub().start()
    yield stub
    stub.stop()


def _apply_mail_import_source_trust_env(env: dict, request) -> None:
    """Seed the source-IMAP trust anchor so a mail-import walk can reach a
    harness-run IMAP server.

    `e2e-automation-surface-gating.md` § The source-IMAP trust seed owns the
    contract. The short version: `mailbox-migration.md` § The two TLS modes
    leaves a source session no plaintext variant — the user's foreign-mailbox
    password crosses it — and no harness can mint a publicly-chained cert for
    `localhost`, so without this the wizard's walk is not slow or awkward, it is
    impossible. `FAUNA_E2E_IMAP_EXTRA_CA_PEM` ADDS the session CA to the WebPKI
    roots inside `NativeImapConnector::new`; it is not an accept-any switch, and
    it exists only in a test-capable build.

    **Marker-gated, unlike the R14 seed**, which is default-on. That one seeds a
    trust fact every test needs and nothing observes negatively; this one points
    the app at a specific harness CA, which is only ever meaningful to a test
    that runs a source server. Off by default keeps every other suite's client
    on exactly the production trust store.

    Session-wide, like `real_conversations`: the app drivers are session-scoped
    (`_driver_cache`) and the anchor is read at connect time out of the launch
    environment, so a per-test opt-in could not seed an app already running.

    One declared absence: **web** runs in wasm, terminates its own TLS inside
    the browser over the byte relay, and has no `std::env::var` to read — a
    different impl of the seam entirely, so there is nothing here to be absent
    from.
    """
    if not any(
        item.get_closest_marker("mail_import_source")
        for item in request.session.items
    ):
        return
    sys.path.insert(0, str(Path(__file__).parent / "fakes"))
    from fake_imap_source import session_ca_pem

    # The CA, never the served leaf. Seeding the leaf makes the app's rustls
    # verifier refuse the handshake with `CaUsedAsEndEntity` — see
    # `fake_imap_source.mint_localhost_chain`.
    env["FAUNA_E2E_IMAP_EXTRA_CA_PEM"] = session_ca_pem()


def _apply_real_conversations_env(env: dict, request) -> None:
    """Session-wide: if any selected test carries the `real_conversations` marker,
    set the env flags that make a native app (windows / macOS / iOS) launch the
    REAL shared-Rust `ConversationsSession` receive loop (real-decrypted mail / MLS
    channel messages) instead of its deterministic mock backend.

    linux/web run the real session for every e2e login by construction; the native
    apps default to the mock backend so the `conversations_inject_inbound` DM tests
    stay deterministic, and opt into the real loop only under this flag. Read by
    windows `App.xaml.cs`, macOS/iOS `applySessionPatch` (`FaunaE2E.realConversations`),
    and — for `FAUNA_CONV_POLL_SECS` — the shared Rust `start_receive_loop` (shrinking
    the 30 s backstop ticker so the INBOX/Sent feed drains in seconds rather than
    relying solely on the `fauna.mail.received` push wake). Session-wide because the
    native apps are session-scoped (`_driver_cache`); run such tests in their own
    invocation so the flag doesn't reach the mock-inject DM tests.
    """
    if any(
        item.get_closest_marker("real_conversations")
        for item in request.session.items
    ):
        env["FAUNA_E2E_REAL_CONVERSATIONS"] = "1"
        env["FAUNA_CONV_POLL_SECS"] = "2"


#: The log target each app's share glue writes under — the BINARY crate's name,
#: which is not always the package name (`fauna-linux` ships the bin
#: `fauna-desktop`, so its root module path is `fauna_desktop`). An app joins
#: the share plane by adding its entry here and calling
#: [`_apply_share_plane_env`] from its branch of the app-config builder.
_SHARE_GLUE_LOG_TARGETS = {
    "tui": "fauna_tui::share_glue",
    "linux": "fauna_desktop::share_glue",
}


def _apply_share_plane_env(env: dict, app_name: str) -> None:
    """The share plane's three e2e knobs, for every app with a landed leg.

    These lived inside the tui branch until 2026-08-21 and cost the linux leg a
    1h46m red the day it landed: the journey is
    app-parametrized, but the env was not, so the linux seats ran with the
    PRODUCTION advertise floor — one hour — and the barrier's 240 s ceiling
    could never see a re-send. The run was also undiagnosable, because the
    share crates' debug logging is part of this same block. Parametrizing a
    journey over a second app therefore means parametrizing its ENV too; this
    helper is that, made impossible to forget by being one call.

    * ``FAUNA_SHARE_PUMP_SECS`` — the pump's cadence knob
      (``share_glue::resolve_pump_secs``, the ``FAUNA_CONV_POLL_SECS``
      pattern): the production 60 s tick is a courtesy cadence, and a two-seat
      peer-transfer journey waiting on it spends minutes per pass. Inert for
      any session with no bound cross-user set (the pump's spec read
      short-circuits on an empty serve list).
    * ``FAUNA_SHARE_ADVERTISE_FLOOR_SECS`` — the floor is the ONLY re-send a
      lost advertisement gets (best-effort by design; production heals in
      ≤1 h). At the production hour, one advertisement lost in flight — a send
      racing the channel's registration, a decrypt the member's walk skipped —
      IS the verdict; at 10 s it is a latency under the assert's ceiling
      (``share_pump::advertise_floor_secs``).
    * ``RUST_LOG`` — the plane's own decisions log at debug (a per-pass failure
      is production's "ordinary weather" but an e2e failure's whole diagnosis);
      scoped to the share crates so transport frames don't drown the stderr the
      harness captures. Merged with whatever the app's branch already set,
      never replacing it.
    """
    env["FAUNA_SHARE_PUMP_SECS"] = "5"
    env["FAUNA_SHARE_ADVERTISE_FLOOR_SECS"] = "10"
    glue_target = _SHARE_GLUE_LOG_TARGETS.get(app_name)
    share_targets = ",".join(
        t
        for t in (
            glue_target,
            "fauna_sync_engine::share_glue=debug",
            "fauna_sync_engine::share_pump=debug",
            "fauna_peer_share=debug",
            # The direct-spawned agent inherits this env: its engine lifecycle
            # + local-write loop are the serve side's whole offline story, and
            # an offline-author flake with no agent line is undiagnosable.
            "fauna_sync_agent=debug",
            "fauna_sync_engine::always_resident=debug",
            "fauna_sync_engine::engine_lifecycle=debug",
            # The CACHED WRITER ROSTER's own decisions. `accept_peer_row`
            # consults this cache before it weighs a row at all, so an empty
            # one refuses every row every peer serves — the plane transfers
            # nothing, silently, for as long as it stays empty. Without this
            # target the refusal is visible (share_pump says it) but its CAUSE
            # is not: whether the roster was read and named no writer, or was
            # never read, or was cleared, are the same observation from
            # outside. Not covered by any target above — the refresh runs in
            # the direct-spawned AGENT, and its skip arms live in
            # `fauna_sync_engine::engine`, which is too broad to raise whole
            # (the roster's four lines carry this target explicitly).
            "fauna_sync_engine::peer_share_store=debug",
            # The GENERATION machinery's own decisions. The sink's dial-row
            # write is a `GenerationTip`-sealed state kind, so "dial row not
            # persisted" can mean a first-need mint refused (no escrow
            # target / no trusted holder / no reachable escrow door) — and
            # without these targets that whole family is one opaque warn
            # (measured 2026-08-24: the linux journey's third distinct red
            # was invisible behind exactly this).
            "fauna_sync_engine::account_state_plane=debug",
            "fauna_sync_engine::generation_mint=debug",
            "fauna_sync_engine::generation_tip=debug",
            # The SEND side of the plane, and the lock it contends for. An
            # advertisement's whole journey is: take the per-channel lock →
            # epoch takeover (a gated commit, up to `MAX_GATE_ROUNDS` rounds,
            # each a replica CAS-put + a catch-up walk) → seal → blind append.
            # Every stage of that was dark, so run 3 (2026-08-24) could see the
            # pipeline sit idle ~8 minutes and then deliver at teardown without
            # being able to say WHICH stage ate them — "the sends were deferred
            # all run" and "the receiving poll was stuck" are the same
            # observation without these. `poll_folder_feed` takes that same
            # lock across its whole inbound walk, so the two are genuinely
            # rival explanations and the timestamps around the acquire are what
            # separate them.
            "fauna_client_mls_sync::commit_gate=debug",
            "fauna_client_mls_sync::gate_impl=debug",
            "fauna_conversations::backends::fauna_mls=debug",
            "fauna_conversations::session=debug",
            # WHICH CONNECTION the plane's writes ride, and whether it is up.
            # The sink's dial row is a `GenerationTip` kind, so it door-mints,
            # and the mint's escrow deposit is the ONE synchronous nest call
            # on the whole plane path — every other leg is local-first and
            # retries. So a data path whose connection never came up is
            # invisible everywhere except that deposit, which is exactly what
            # run 7 measured (`rpc disconnected (was_in_flight=false)`, 272x)
            # and could not explain. These two targets carry the answer: which
            # client the runtime chose (`resolve_and_start`) and whether that
            # client ever connected (`spawn_connect_retry`). Both logged
            # nothing usable before 2026-08-26 — the success paths were silent
            # and the failure path sat at debug on a target nothing enabled.
            "fauna_client_account_runtime=debug",
            "fauna_client::ws_device_handshake_bearer=debug",
            # …and WHY that connection is refused. Run 8 (2026-08-26) settled
            # the first half — the principal's client is refused
            # `fauna.auth.not_registered` on every attempt, so the data path
            # never comes up — and the second half is the enrollment leg that
            # was supposed to register the `RenewBearer` grant. That leg is
            # silent by construction: `EnrollmentPass::Unenrolled` is a
            # legitimate value nothing logs, and a failing grant RPC lands in
            # `PumpReport::errors`, which no log prints. Both now name
            # themselves on this target.
            "fauna_sync_engine::account_runtime=debug",
        )
        if t
    )
    if glue_target:
        share_targets = share_targets.replace(glue_target, f"{glue_target}=debug", 1)
    existing = env.get("RUST_LOG")
    env["RUST_LOG"] = f"{existing},{share_targets}" if existing else f"info,{share_targets}"


#: The engine's held-pass line (`SyncEngine::pull_remote_changes`, its own
#: target) — the seat's only account of a pull its place declined.
PLACE_ACCEPTS_LOG_TARGET = "fauna_sync_engine::place_accepts"
PLACE_ACCEPTS_HELD_LINE = "place does not accept remote changes; holding the anchor"


def _apply_place_accepts_log_env(env: dict) -> None:
    """Turn on the engine's held-pass line in the direct-spawned agent, which
    inherits this env. `local-folder-sync` outcome 15's journey counts it: a
    pass that declined AFTER the held files reached the nest is the
    state-defined moment at which "this device stopped receiving them" can be
    asserted (convention 14), with no wall clock. One line per declined pass,
    and only on a seat whose place does not accept, so it is cheap to leave on
    for every launch. Merged into whatever `RUST_LOG` the branch already set."""
    target = f"{PLACE_ACCEPTS_LOG_TARGET}=debug"
    existing = env.get("RUST_LOG")
    env["RUST_LOG"] = f"{existing},{target}" if existing else f"info,{target}"


def _apply_actuation_mode_env(env: dict, request) -> None:
    """An in-process automation server must not actuate a control the real UI has
    disabled — and `--permissive-actuation` opts back out of that.

    Applied to apple (refusal is the DEFAULT there since 2026-08-05), to tui
    (refusal is the DEFAULT there too — it gated `click` from the start, and all
    five routes since 2026-08-21), to linux (refusal the DEFAULT since its
    2026-09-10 sweep, which found the gate's own probe as the only violating
    call) and to windows (refusal the DEFAULT since 2026-09-14, after its
    2026-09-11 chunked sweep). All four read the same three
    environment variables — the Rust half is shared in `libs/fauna-e2e-agent`
    (`gate_actuation`, hosted by both tui and linux), the Swift half in
    `InProcessAutomationServer`, and windows' FlaUI bridge mirrors the Rust half
    in `flaui-bridge/ActuationGate.cs` — so one sweep invocation spans all of
    them, and one grep spans their logs.

    The five actuation routes (`/element/{click,double_click,type,clear,select}`)
    invoked their registered closure without ever consulting `Entry.isEnabled`,
    so the apple harness could activate a `.disabled(...)` control — a
    harness-only capability with no user analogue, and a silent divergence from
    web (Playwright's `click()` auto-waits for enabled and fails loudly). It is
    testing.md convention 11 one layer down: not a *dropped* command but an
    *illegal* one silently honoured, whose downstream failure reads exactly like
    a product bug.

    It was staged rather than flipped, because ~170 `isEnabled:` registrations
    exist across the two apple targets and turning refusal on blind could have
    reddened an unknown number of green tests. That staging is OVER (2026-08-05):
    both targets were swept permissively end to end, every offender triaged, and
    the remaining violation on each is the gate's own probe driving a disabled
    control on purpose. So refusal is the default and this flag is the way OUT of
    it, not the way in.

    `--actuation-log PATH` is the whole-suite harvest, and it composes with this
    flag deliberately the other way round from what one might guess: the
    enumerating run is the PERMISSIVE one. A strict red stops its test at the
    FIRST offender and never reveals the rest, whereas a permissive pass turns
    nothing red, so one sweep yields both the complete offender list and the
    pre-existing-red baseline a flip decision must subtract. That is why the
    permissive mode survives the flip rather than being deleted with it: it is
    the measuring instrument, and the next broad change to either apple app
    re-measures itself the same way. Session-wide because the native apps are
    session-scoped (`_driver_cache`), exactly like the two flags above.
    """
    if request.config.getoption("--permissive-actuation"):
        env["FAUNA_E2E_PERMISSIVE_ACTUATION"] = "1"
    log_path = request.config.getoption("--actuation-log")
    if log_path:
        env["FAUNA_E2E_ACTUATION_LOG"] = str(Path(log_path).expanduser().resolve())
    # The opt-IN, forwarded EXPLICITLY rather than left to process inheritance.
    # `FAUNA_E2E_STRICT_ACTUATION` is what a still-STAGING host is exercised
    # strictly with — none stages today, windows having flipped last
    # (2026-09-14) — and it has no pytest option because a human
    # exports it into the shell for a spot check. Inheritance is enough only where
    # the app reads its own process environment (linux, tui). It is NOT enough for
    # windows, whose gate lives in the FlaUI bridge and reads the launch dict this
    # helper fills, and it is not enough for the simulator/child-env forwarding
    # apple's targets go through either. Forwarding it here makes the opt-in reach
    # every app by the same one path the other two flags already take.
    strict = os.environ.get("FAUNA_E2E_STRICT_ACTUATION")
    if strict:
        env["FAUNA_E2E_STRICT_ACTUATION"] = strict


def _selected_drafts_autosave_window_ms(session) -> int | None:
    """The single window value this session's selected tests ask for, or None.

    Session-wide by necessity, like every other launch-env flag here: the app
    drivers are session-scoped (`_driver_cache`), so a per-test marker could not
    re-launch an app that is already running. Two different values in one
    selection is a contradiction rather than a preference, so the caller raises.
    """
    wanted = {
        int(m.args[0])
        for item in session.items
        for m in item.iter_markers("drafts_autosave_window_ms")
    }
    if len(wanted) > 1:
        raise pytest.UsageError(
            "drafts_autosave_window_ms: this selection asks for "
            f"{sorted(wanted)} ms at once, but the app drivers are "
            "session-scoped (`_driver_cache`) so one launch serves the whole "
            "run. Select one window per invocation."
        )
    return wanted.pop() if wanted else None


def _apply_drafts_autosave_window_env(env: dict, request) -> None:
    """Session-wide: `@pytest.mark.drafts_autosave_window_ms(N)` on any selected
    test sets every app launch's draft-autosave debounce window to N ms.

    The seam is one shared accessor — `fauna_client_drafts::autosave_debounce`,
    which `fauna-ffi`'s `autosave_debounce_ms` is the apple/android/windows door
    to — so this one marker reaches every app that takes a launch env dict
    without a line of app-side plumbing (priority #2;
    `docs/goal/architecture/e2e-automation-surface-gating.md` § The source-IMAP
    trust seed is the ratified shape).

    **It exists to LENGTHEN the window, not shorten it.** A leave-flush witness
    has to prove the draft reached the nest through the app's leave door rather
    than through the debounce landing on its own. Every such leg but iOS's is
    sound because the process is gone by then, so the debounce provably cannot
    have fired; iOS's door leaves the app RUNNING, so its leg instead pushes the
    window out past the whole test and any save at all is necessarily the flush.
    Shortening it to race the debounce is what convention 14 forbids — under the
    production 1.5 s it only trades a false green for a load-dependent false red
    (measured pass/fail/pass/fail over four runs before this seam existed).

    Scope: the three apps whose launch takes an env dict (ios via simctl
    `SIMCTL_CHILD_*`, macos via the binary's env, windows via the FlaUI bridge's
    `/session` body). linux and tui read their own process environment, so a
    future leg of theirs would want the value exported around the launch instead
    — they need none today, their doors being real exits. web cannot be reached
    this way at all: `wasm32-unknown-unknown` has no process environment (see
    `fauna-wasm`'s `autosaveDebounceMs`).

    Co-selection is REFUSED at collection rather than documented away
    (`_refuse_drafts_window_collision`). The sibling flags above only swap a
    backend, so a stray co-selection degrades a run; a lengthened window instead
    makes every restart-round-trip drafts test fail outright, and this row's
    whole job is to keep the catalog honest — four reds recorded as product
    verdicts is a far worse outcome than one clear refusal.
    """
    ms = _selected_drafts_autosave_window_ms(request.session)
    if ms is not None:
        env["FAUNA_E2E_DRAFTS_AUTOSAVE_DEBOUNCE_MS"] = str(ms)


def _macos_photo_library_venue(request) -> bool:
    """Whether this run's macOS launches are the photo-library venue — the
    real-session launch against the REAL System Photo Library
    (`drivers/macos.py` § the photo-library venue; `e2e-conventions.md`
    convention 12's macOS arm).

    Session-wide like `real_conversations`, because the macOS driver is
    session-scoped (`_driver_cache`) — which is exactly why a mixed selection is
    refused rather than resolved: an ordinary macOS test swept into the venue would
    run under the fixed test bundle id against the machine-global library it has no
    business near (convention 10). The marker lives only in `tests/real_session/`,
    which is pruned unless `--real-session` is passed, so an ordinary run never
    reaches this with anything to find.
    """
    items = request.session.items
    if not any(i.get_closest_marker("real_photos_library") for i in items):
        return False
    strays = [i.nodeid for i in items
              if i.get_closest_marker("macos")
              and not i.get_closest_marker("real_photos_library")]
    if strays:
        raise pytest.UsageError(
            "a `real_photos_library` run launches EVERY macOS app of the session as the "
            "photo-library venue (the driver is session-scoped), so it must run alone; "
            f"these macOS tests would be swept in: {strays[:5]}"
        )
    return True


def _apply_real_sync_agent_env(env: dict, request) -> None:
    """Session-wide: if any selected test carries the `real_sync_agent` marker, set
    the flag that makes the windows app's set_state login start the session-scoped
    HydrationSessionService (App.xaml.cs) — probe the per-SID fauna-sync pipe, SPAWN
    fauna-sync-agent.exe if absent, and provision it with a real capability. Off by
    default: a deterministic e2e must never spawn or write into the box's installed
    agent (see the installer journey's foreign-agent guard,
    helpers/windows_sync_agent.py). Used by the installer full journey
    (tests/platform/windows/test_installer.py), which drives the MSI-installed app +
    agent end-to-end. Session-wide because the windows app is session-scoped
    (`_driver_cache`); run such tests in their own invocation."""
    if any(
        item.get_closest_marker("real_sync_agent") for item in request.session.items
    ):
        env["FAUNA_E2E_REAL_SYNC_AGENT"] = "1"


def _apply_macos_sync_agent_bin(env: dict, request) -> None:
    """Pin WHICH agent binary a macos launch may spawn, when one will be spawned.

    The second half of macOS' real-agent harness, and never separable from
    :func:`_apply_real_sync_agent_env`'s first half: macOS builds the app with
    SwiftPM (``apps/fauna-apple/.build/``) and the agent with cargo
    (``target/debug/``), so they are never siblings — the app's
    sibling-of-exe probe misses and the bare-name fallback resolves against
    PATH, silently starting the box's INSTALLED ``/usr/local/bin`` agent, which
    is stale, machine-global and shared with the developer's own session
    (``testing.md`` point 10).

    A helper rather than two lines inlined at each call site because there are
    two: ``_build_app_config``'s macos arm, and the modules that hand-build a
    launch config instead of going through it
    (``test_filesync_delete_declined_debounce.py``). The copy-by-hand shape is
    what it replaces — omitting this half there is a run that spends its whole
    budget polling an empty listing with no error anywhere (2026-09-21).
    No-op unless the first half turned the harness on.
    """
    if env.get("FAUNA_E2E_REAL_SYNC_AGENT"):
        env["FAUNA_E2E_SYNC_AGENT_BIN"] = str(
            request.getfixturevalue("macos_sync_agent_binary")
        )


@pytest.fixture(scope="session")
def sync_agent_binary():
    """Build fauna-sync-agent.exe once per session, then assert the exe exists.

    The one real-agent binary build every windows real-agent e2e shares — the
    direct-IPC suite (tests/platform/windows/test_per_user_sync_agent.py) and the
    `isolated_sync_agent` UI harness (test_sync_live_apply.py) both depend on this
    fixture rather than each building their own — one spawner, not two.

    Delegates to `_ensure_sync_agent_built` (the memoized layer shared with the
    collection-time prebuild hoist) → `common.nest.build_sync_service_win`,
    which owns the build/freshness mechanics: a machine-wide `build` slot
    (previously ran completely unslotted), a warm-tree stamp that skips cargo
    entirely, the ROOT-workspace/implicit-host build shape (build-machine-
    resources.md § Cargo target dir layout (win) → *The host-side builds ask
    for their own triple*; row 60, 2026-08-23), and the win-arm64
    cargo-exit-code caveat (judged by the known exe path existing, never the
    return code).
    """
    return _ensure_sync_agent_built()


_MACOS_AGENT_BIN = _repo_root / "target" / "debug" / "fauna-sync-agent"


@pytest.fixture(scope="session")
def macos_sync_agent_binary():
    """Build the host `fauna-sync-agent` once per session, then assert it exists.

    The macOS twin of :func:`sync_agent_binary` (windows), and the value behind
    ``FAUNA_E2E_SYNC_AGENT_BIN`` for a macos launch carrying `real_sync_agent`.

    It has to be an explicit fixture rather than "whatever the app finds"
    because the two halves live in different build systems: `just mac-debug`
    produces the app under `apps/fauna-apple/.build/`, while the agent is a
    cargo binary in the root `target/debug/`. They are never siblings, so the
    shared sibling-of-exe probe misses and the bare-name fallback resolves
    against PATH — which on a dev box finds the INSTALLED
    `/usr/local/bin/fauna-sync-agent`. That is how a macos e2e launch could end
    up driving a stale, machine-global agent shared with the developer's own
    session (testing.md point 10) instead of the code under test.

    Build goes through the machine-wide build slot like the `just` recipes do,
    so it queues behind concurrent builds instead of oversubscribing the box.
    The slot script is fleet-only tooling and does not ship — a solo checkout
    falls back to an unslotted build.
    """
    _slot = _repo_root / "scripts" / "build-slot.py"
    _cmd = (
        [sys.executable, str(_slot), "--pool", "build", "--"] if _slot.exists() else []
    ) + ["cargo", "build", "-p", "fauna-sync-agent"]
    subprocess.run(_cmd, cwd=_repo_root, check=False)
    if not _MACOS_AGENT_BIN.exists():
        pytest.fail(
            f"fauna-sync-agent not found at {_MACOS_AGENT_BIN}\n"
            "A macos test carrying the `real_sync_agent` marker needs the real "
            "agent binary. Build it with:\n"
            "  cargo build -p fauna-sync-agent"
        )
    return str(_MACOS_AGENT_BIN)


@pytest.fixture(scope="session")
def isolated_sync_agent_pipe_name():
    r"""Per-session pipe leaf name for the `isolated_sync_agent` e2e seam.

    Matches the leaf-name form `SyncServicePipeClient.PipeName`'s `FAUNA_E2E_SYNC_PIPE`
    override expects (no `\\.\pipe\` prefix — .NET's `NamedPipeClientStream` takes the
    leaf; the Rust agent's `--pipe-name` CLI arg wants the full path, so callers that
    spawn the agent directly — `helpers.windows_sync_agent.running_agent` — must build
    ``r"\\.\pipe\" + isolated_sync_agent_pipe_name`` themselves). The windows app is
    session-scoped (`_driver_cache`), so this can't vary per test — one name per
    session is enough to keep a real-agent run off the machine's per-SID pipe.
    """
    return f"fauna-sync-e2e-{os.getpid()}"


@pytest.fixture(scope="session")
def isolated_sync_agent_data_dir(tmp_path_factory):
    """Per-session `--data-dir` for an APP-spawned isolated agent.

    Session-scoped for the same reason as `isolated_sync_agent_pipe_name`: the
    windows app is session-scoped (`_driver_cache`), so its launch env — and
    therefore the data dir it forwards to any agent it spawns — cannot vary per
    test. One session, one pipe, one data root, matching by construction.

    Every reader of the launch's agent state reads it through
    `driver.sync_agent_state_base`, and a fixture that brings the agent up before
    login does so on this dir (`helpers/windows_sync_agent.serving_agent`).
    Only the direct-IPC suites that launch no session app spawn agents on dirs
    of their own.
    """
    d = tmp_path_factory.mktemp("isolated-sync-agent-data")
    (d / "credentials").mkdir(exist_ok=True)
    return str(d)


#: **The 2026-09-21 ruling, BUILT 2026-09-22.** `e2e-conventions.md` convention 10
#: (windows axis (a)) RULES that a windows e2e launch drives its own isolated
#: agent by default, and this is where that default lives: the app's gate keys on
#: the pipe+binary pin (`App.xaml.cs::HydrationSessionEnabled`), the launch env
#: pins all four variables, and the collection-time prebuild hoist reads the
#: predicate below so `fauna-sync-agent.exe` is built before any test's clock
#: starts.
#:
#: It sat staged ``False`` for a day on a gap that turned out to be windows' own:
#: the provisioner was built on a one-shot `new FfiNestClient(...)` that was never
#: `Connect()`ed, so its renewal-grant mint's `fauna.sync.register` waited out
#: its deadline and failed as "the connection to the nest was lost" — the mint
#: decided and reached, the named row never landed. It now builds on the login's
#: own connected client (`NestRpcClient.BuildSyncAgentProvisionerAsync`), as macOS
#: does on `ensureNestConnected()`. Opt out per suite with ``no_sync_agent``.
_WINDOWS_ISOLATED_AGENT_IS_DEFAULT = True


def windows_isolates_its_sync_agent(items) -> bool:
    """Does this invocation give the windows app its OWN sync agent?

    The single home of the 2026-09-21 default (`e2e-conventions.md` convention
    10), read by both consumers: the launch env that pins the pipe + binary
    (:func:`_apply_isolated_sync_agent_env`) and the collection-time prebuild
    hoist that must build `fauna-sync-agent.exe` BEFORE any test's clock starts.
    Two readers of one rule, never two spellings of it — the hoist drifting from
    the env is precisely how a cold cargo build lands inside a 900 s test budget.

    ``isolated_sync_agent`` on any selected test always wins — which is what makes
    ``real_sync_agent`` + ``isolated_sync_agent`` mean "a real-agent UI test on our
    own pipe". The two other postures are:

    * ``real_sync_agent`` alone — the installed-product journey, which must
      rendezvous on the machine-global per-SID pipe;
    * ``no_sync_agent`` — no agent at all.

    With :data:`_WINDOWS_ISOLATED_AGENT_IS_DEFAULT` ``True`` (since 2026-09-22), a
    run naming none of the three isolates too — the ruled default.
    """

    def any_marker(name: str) -> bool:
        return any(item.get_closest_marker(name) for item in items)

    if any_marker("isolated_sync_agent"):
        return True
    if not _WINDOWS_ISOLATED_AGENT_IS_DEFAULT:
        return False
    return not (any_marker("real_sync_agent") or any_marker("no_sync_agent"))


def _apply_isolated_sync_agent_env(env: dict, request) -> None:
    r"""Session-wide: redirect the windows app's agent pipe (`FAUNA_E2E_SYNC_PIPE`)
    to this run's own per-session pipe name (`isolated_sync_agent_pipe_name`), so
    the shared provisioner (App.xaml.cs's `SyncAgentSession`) probes and provisions
    ONLY the agent this run itself spawned — never the box's installed or a sibling
    checkout's per-SID agent (testing.md § conventions point 10).

    **Ruled the default 2026-09-21, in force since 2026-09-22** — the switch and
    its history are :data:`_WINDOWS_ISOLATED_AGENT_IS_DEFAULT`. The rule
    convention 10 states is *never the box's agent*, not *never an agent*. linux and tui both spawn the real agent binary
    as an isolated per-launch child under e2e (`apps/fauna-linux/src/sync_agent.rs`
    § e2e; `drivers/tui.py` sets this very pipe by `setdefault` on win32), so
    windows alone was not exercising the production sync bring-up — and since the
    machine's named `sync_devices` row is registered by that bring-up's
    renewal-grant mint (`fauna_client_sync::setup_renewal_grant`), no windows e2e
    could assert a named-row invariant at all: `test_sign_out_device_roster.py`
    had to be weakened to a subset on 2026-09-21 for exactly that reason. Owner: `e2e-conventions.md` convention 10.

    **The two ways out**, both deliberate:

    * a selected test carries `real_sync_agent` WITHOUT `isolated_sync_agent` —
      the installer full journey, which tests the INSTALLED product and therefore
      must rendezvous on the machine-global per-SID pipe. Redirecting it would
      make the journey assert against an agent the installer never placed, and
      its foreign-agent guard (`helpers/windows_sync_agent.blocking_diagnosis`)
      is what keeps the box honest for it.
    * a selected test carries `no_sync_agent` — "this run must spawn no agent at
      all". With the app's gate keyed on the pipe+binary pin
      (`App.xaml.cs::HydrationSessionEnabled`), withholding the pin IS the
      opt-out: the provisioner never starts.

    The windows app is session-scoped (`_driver_cache`), so all three postures are
    per-INVOCATION, never per-test; a suite needing a posture other than the
    default runs in its own invocation.

    Also pins WHICH agent binary the app may spawn (`FAUNA_E2E_SYNC_AGENT_BIN` —
    the same env name shared Rust uses, `libs/fauna-client-sync` `AGENT_BIN_ENV`)
    and WHERE that agent keeps its state (`FAUNA_E2E_SYNC_AGENT_DATA_DIR`). Both
    are needed for an app-spawned agent to be isolated on windows: the unix
    clients' agent socket is path-derived, so a private HOME/XDG_RUNTIME_DIR
    isolates it for free, but windows rendezvouses on the machine-global
    `\\.\pipe\fauna-sync.<SID>` and defaults its state to `%LOCALAPPDATA%`, so
    both must be passed explicitly (App.xaml.cs `SpawnSyncAgentDetached` forwards
    them as `--pipe-name` / `--data-dir`). Without the pin, a dev tree stages no
    agent beside the app and the spawn silently does nothing — the bug that made
    the multiseat windows seat look bound while syncing in neither direction
    (2026-07-24).

    A fixture that needs the agent up before login (`custodian_pull_app`,
    `sync_live_apply_app`, the live-engine media fixture) brings up THIS agent —
    on this `--data-dir`, through `helpers/windows_sync_agent.serving_agent` —
    never a second one of its own: a second agent on the pipe exits as a
    duplicate the moment the launch's already serves it.
    """
    if not windows_isolates_its_sync_agent(request.session.items):
        return
    env["FAUNA_E2E_SYNC_PIPE"] = request.getfixturevalue("isolated_sync_agent_pipe_name")
    env["FAUNA_E2E_SYNC_AGENT_BIN"] = request.getfixturevalue("sync_agent_binary")
    data_dir = request.getfixturevalue("isolated_sync_agent_data_dir")
    env["FAUNA_E2E_SYNC_AGENT_DATA_DIR"] = data_dir
    # The agent's credential store is env-routed, NOT data-dir-routed: without
    # this an app-spawned agent restores the INSTALLED product's persisted
    # capability (a real-nest URL) and fights 401s against it. Same reasoning
    # as helpers/windows_sync_agent.running_agent, which sets it for the
    # test-spawned case.
    env["FAUNA_E2E_CREDENTIAL_DIR"] = str(Path(data_dir) / "credentials")


def _build_app_config(app_name: str, nest_instance: dict, request) -> dict:
    """Build launch config for a client. Shared by driver cache and persistent_app.

    ``nest_instance`` is the session nest dict — or, in a `live_nest` session,
    a ``{"url": <live nest url>}`` stub (only ``["url"]`` is ever read here; a
    live session builds no local nest — see ``_live_nest_session``)."""
    from helpers.app_surface import skip_unbuilt

    # The installer full-journey suite drives the MSI-INSTALLED app, so building
    # the DEV app it will never launch is pure waste — and not cheap waste: `just
    # windows-debug` is a full WinUI MSBuild that blew the 900 s per-test timeout
    # on the first live run of this suite. Skipped, not reordered: the override
    # below already resolves the app path, and nothing else in the closure
    # consumes the dev build (the installed app is prebuilt and reads none of the
    # repo's generated files at runtime; the nest's own generated-file freshness
    # is `_generated_files_fresh`'s job, not this recipe's).
    if not (app_name == "windows" and _installed_windows_app_override(request)):
        _ensure_app_built(app_name)
    if app_name == "ios":
        ios_setup = _get_ios_setup()
        if ios_setup is None:
            skip_unbuilt(
                "ios",
                surface="the iOS simulator/app",
                detail="run 'just apple-ffi-test' first",
            )
        # Wire the e2e fake DNS-provider seam (same sentinel-gated hook as
        # linux/windows/macos) so the tier_2 managed-publish harness drives
        # verify()/publish() with no real DNS provider. The ios driver forwards
        # config["environment"] via simctl SIMCTL_CHILD_* (drivers/ios.py);
        # the in-process Rust FFI (fauna-client-dns
        # build_dns_management_machine_with_credentials) reads it. Production
        # never sets it; transparent for non-sentinel creds. The
        # real_conversations marker adds FAUNA_E2E_REAL_CONVERSATIONS (+ poll knob),
        # mirroring windows/macos.
        env = {"FAUNA_DNS_PROVIDER_FAKE": "1"}
        _apply_real_conversations_env(env, request)
        _apply_actuation_mode_env(env, request)
        _apply_drafts_autosave_window_env(env, request)
        _apply_r14_trust_env(env, nest_instance, request)
        _apply_mail_import_source_trust_env(env, request)
        _apply_release_feed_env(env, request)
        return {
            "url": nest_instance["url"],
            "app_path": ios_setup["app_path"],
            "udid": ios_setup["udid"],
            "device_name": "iPhone 17 Pro",
            "platform_version": "26.4",
            "environment": env,
        }
    elif app_name == "macos":
        # Launch the BARE swift-build binary directly — NOT a ~/Applications
        # .app bundle. Such a bundle was vestigial XCUITest scaffolding (it
        # existed so Launch Services + XCUITest could discover the app by the
        # fixed bundle ID social.fauna.fauna; the wrapper was deleted once the
        # root cause below was understood); XCUITest was retired at the
        # in-process cutover, so the in-process driver — which
        # Popen-launches the binary and talks to its in-app HTTP agent — never
        # needs it. Worse, the bundle is the ROOT CAUSE of the so-called macOS
        # "render quirk": an app launched under the FIXED bundle ID
        # social.fauna.fauna reliably stops getting a WindowServer-backed
        # window after enough launch-and-die cycles (process alive, /health 200,
        # automation server listening, but CGWindowListCopyWindowInfo == 0 → empty
        # AutomationRegistry → every element 404s). It looks like a degraded GUI
        # session "needing a reboot", but it is purely bundle-ID-scoped: the bare
        # binary (whose ad-hoc identifier is per-build, not a fixed bundle ID)
        # renders 100% of launches at the same load on the same VM — proven by
        # isolation (bare 5/5 healthy vs bundle 0/5, setsid irrelevant). The
        # render-readiness preflight (assert_render_ready) stays as a cheap
        # tripwire, but with the bare-binary launch it should never fire.
        # `--macos-artifact` swaps the subject from the bare binary to the shipped
        # `.app` bundle. The driver stages a per-instance copy of it (see
        # `drivers/macos.py` `launch_mode`), which is what keeps the fixed-bundle-id
        # wedge described below from coming back with it.
        if _MACOS_ARTIFACT_MODE:
            from helpers.macos_artifact import ARTIFACT_APP_RELPATH

            app_path = _repo_root / ARTIFACT_APP_RELPATH
            if not app_path.exists():
                skip_unbuilt(
                    "macos",
                    surface=f"the macOS app bundle (expected at {app_path})",
                    detail="run 'just mac-app debug' first",
                )
        else:
            app_path = _repo_root / APP_PATHS["macos"]
            if not app_path.exists():
                skip_unbuilt(
                    "macos",
                    surface="the macOS app binary",
                    detail="run 'just mac-debug' first",
                )
        # Wire the e2e fake DNS-provider seam (same sentinel-gated hook as
        # linux/windows below) so the tier_2 managed-publish harness
        # (test_admin_dns_managed.py / cert_delegate / cert_auto_renew) drives
        # verify()/publish() with no real DNS provider. The macos driver
        # forwards config["environment"] into the launched binary's env
        # (drivers/macos.py), where the in-process Rust FFI
        # (fauna-client-dns build_dns_management_machine_with_credentials)
        # reads it. Production never sets it; transparent for non-sentinel creds.
        # The real_conversations marker adds FAUNA_E2E_REAL_CONVERSATIONS (+ poll
        # knob), mirroring windows/ios.
        env = {"FAUNA_DNS_PROVIDER_FAKE": "1"}
        _apply_real_conversations_env(env, request)
        _apply_actuation_mode_env(env, request)
        _apply_drafts_autosave_window_env(env, request)
        # Real-sync-agent harness (the `real_sync_agent` marker): launch with
        # FAUNA_E2E_REAL_SYNC_AGENT=1 so FaunaMacApp builds the provisioner with
        # the shared-Rust FfiChildAgentSpawner — a private child agent on THIS
        # launch's socket — instead of skipping the provisioner outright. Without
        # it the macOS app binds folders that have nothing behind them: correct
        # for binding-UI tests, fatal for any test expecting bytes to move (the
        # multiseat seat synced in neither direction, run 20260724-02).
        _apply_real_sync_agent_env(env, request)
        _apply_r14_trust_env(env, nest_instance, request)
        _apply_mail_import_source_trust_env(env, request)
        _apply_release_feed_env(env, request)
        # Pin the child to the freshly-built agent (the harness's second half —
        # see `_apply_macos_sync_agent_bin`, which is also what the modules
        # that hand-build a launch config call).
        _apply_macos_sync_agent_bin(env, request)
        # DERIVED from the driver, never repeated — the same rule the iOS
        # synthetic bundle follows above. A second literal here is precisely the
        # drift `test_apple_identifier_pins.py` exists to prevent: two internally
        # consistent copies of one identifier that nothing can see disagree.
        from drivers.macos import BUNDLE_ID as MACOS_BUNDLE_ID
        from drivers.macos import PHOTO_LIBRARY_BUNDLE_ID

        photo_library = _macos_photo_library_venue(request)
        if photo_library and _MACOS_ARTIFACT_MODE:
            raise pytest.UsageError(
                "`--macos-artifact` and a `real_photos_library` test cannot share a run: "
                "the artifact mode launches the shipped bundle, the photo-library venue "
                "its own stably signed test bundle"
            )
        config = {
            "url": nest_instance["url"],
            "app_path": str(app_path),
            "bundle_id": MACOS_BUNDLE_ID,
            "launch_mode": "bundle" if _MACOS_ARTIFACT_MODE else "binary",
            "environment": env,
        }
        if photo_library:
            # The venue's grant belongs to its own fixed test id, never the
            # shipped app's (`drivers/macos.py` § the photo-library venue).
            config["bundle_id"] = PHOTO_LIBRARY_BUNDLE_ID
            config["launch_mode"] = "photo-library"
        return config
    elif app_name == "web":
        spa = request.getfixturevalue("spa_url")
        # The escrow-trust seed, like every native launch's: web's driver
        # writes it into the origin's localStorage (`drivers/web.py::launch`),
        # which its account runtime reads under `test-helpers`.
        env: dict = {}
        _apply_r14_trust_env(env, nest_instance, request)
        return {"url": spa + "/app/", "environment": env}
    else:
        config = {"url": nest_instance["url"]}
        if app_name == "linux":
            # The installed-product journey drives what `install.sh` put in its
            # prefix, not the build-tree binary — `_installed_linux_app_override`
            # explains why, and raises rather than silently falling back.
            installed_linux = _installed_linux_app_override(request)
            linux_bin = installed_linux or _resolve_linux_binary()
            if linux_bin:
                config["app_path"] = str(linux_bin)
            # Wire the e2e fake DNS-provider decorator (sentinel-gated, transparent
            # for non-sentinel credentials) so the tier_2 managed-publish harness
            # (test_admin_dns_managed.py) can drive verify()/publish() with no real
            # DNS provider. Production never sets this; see fauna-client-dns
            # build_dns_management_machine_with_credentials.
            #
            # FAUNA_CONV_POLL_SECS shrinks the native receive loop's backstop ticker
            # from its 30 s production default so the e2e round-trips surface a
            # delivered message in seconds rather than waiting a production poll
            # cycle. The unified `ConversationsSession::start_receive_loop` drives
            # BOTH rails on this one cadence, so it governs the FaunaMls conversation
            # poll (test_fauna_mls_real_roundtrip.py — channel messages / welcomes)
            # AND the inbound-mail poll (test_mail_client_receive.py — the INBOX /
            # Sent read-feeds, which the linux mail loop formerly read on its own
            # FAUNA_INBOUND_POLL_SECS knob until a later cleanup folded it onto the
            # shared loop). Not a user-facing knob — an internal cadence, overridable
            # only for the e2e. Harmless for non-mail/non-conv tests: the loop no-ops
            # on a rail until it is configured.
            #
            # `RUST_LOG` raises the conversations rail to debug in `app.err`.
            # linux `block_on`s the `real_*` agent commands' nest round-trips on
            # the GTK main thread, so a wedged leg stops the app acking anything
            # at all — and `app.err` is the only witness of WHICH leg (the
            # driver now quotes its tail on an ack timeout, see
            # `drivers/linux.py::ack_timeout_diagnostics`). A prior investigation spent three runs establishing only that a never-acking
            # `conversations_real_resolve_send_new` was not a budget problem,
            # because nothing recorded the phase. A few hundred extra lines per
            # run is a cheap price for that.
            config["environment"] = {
                "FAUNA_DNS_PROVIDER_FAKE": "1",
                "FAUNA_CONV_POLL_SECS": "2",
                "RUST_LOG": "info,fauna_conversations=debug",
            }
            # The share plane's knobs — linux is the plane's second app
            # leg.
            _apply_share_plane_env(config["environment"], app_name)
            _apply_place_accepts_log_env(config["environment"])
            # The actuation gate (convention 11 one layer down), same flags as
            # apple. linux REFUSES by default since its 2026-09-10 sweep, so a
            # whole-suite harvest is `--permissive-actuation --actuation-log
            # PATH` — the permissive mode is what turns nothing red and so
            # enumerates every offender.
            _apply_actuation_mode_env(config["environment"], request)
            _apply_r14_trust_env(config["environment"], nest_instance, request)
            _apply_mail_import_source_trust_env(config["environment"], request)
            _apply_release_feed_env(config["environment"], request)
        elif app_name == "windows":
            config["app_path"] = _installed_windows_app_override(request) or str(
                _build_windows_app()
            )
            # Wire the e2e fake DNS-provider decorator (same sentinel-gated hook as
            # linux above) so the tier_2 managed-publish harness
            # (test_admin_dns_managed.py) can drive verify()/publish() with no real
            # DNS provider. The windows driver forwards config["environment"] to the
            # FaunaApp child process, where the in-process Rust FFI
            # (build_dns_management_machine_with_credentials) reads it. Production
            # never sets this; transparent for non-sentinel credentials.
            env = {"FAUNA_DNS_PROVIDER_FAKE": "1"}
            # Real-conversations harness (the `real_conversations` marker): launch
            # with FAUNA_E2E_REAL_CONVERSATIONS=1 so App.xaml.cs's set_state login
            # path builds the real ConversationsSession + receive loop instead of the
            # mock ConversationsManagerHost. Shared with macos/ios; see the helper.
            _apply_real_conversations_env(env, request)
            # Real-sync-agent harness (the `real_sync_agent` marker): launch with
            # FAUNA_E2E_REAL_SYNC_AGENT=1 so the set_state login starts the
            # session-scoped hydration provisioning loop (spawn + provision the
            # real fauna-sync agent). Installer full journey only; see the helper.
            _apply_real_sync_agent_env(env, request)
            # Isolated-sync-agent harness (the `isolated_sync_agent` marker):
            # redirect FAUNA_E2E_SYNC_PIPE to this run's own pipe, so a real-agent
            # UI test (combined with real_sync_agent above) never touches the box's
            # installed/per-SID agent. See the helper + isolated_sync_agent_pipe_name.
            _apply_isolated_sync_agent_env(env, request)
            # The installer full journey launches the installed app UNPINNED, so
            # it spawns the MSI's own agent (see the helper).
            _apply_installed_product_agent_env(env, request)
            # The actuation gate (convention 11 one layer down), same two flags as
            # apple, linux and tui. Refusal is windows' DEFAULT since 2026-09-14
            # (`flaui-bridge/ActuationGate.cs::
            # WindowsRefusesDisabledActuationByDefault`), so the way OUT is
            # `--permissive-actuation`, and the enumerating whole-suite harvest is
            # `--permissive-actuation --actuation-log PATH`.
            #
            # ⚠ windows' gate lives in the FlaUI bridge, not in the app: every
            # actuation is a UIA call the bridge makes, so the app never sees the
            # route. These vars still ride the app-launch dict because that dict IS
            # the `/session` POST body the bridge receives, and
            # `flaui-bridge/Program.cs` reads the gate out of it there (the same
            # dictionary `SessionManager.Launch` already takes FAUNA_E2E_DATA_DIR
            # and FAUNA_E2E_SESSION_EPOCH from). One conftest hook, every app —
            # which is the point: convention 11's third lesson is that a flag not
            # reaching the app makes a sweep measure nothing while reporting clean.
            _apply_actuation_mode_env(env, request)
            _apply_drafts_autosave_window_env(env, request)
            _apply_r14_trust_env(env, nest_instance, request)
            _apply_mail_import_source_trust_env(env, request)
            _apply_release_feed_env(env, request)
            config["environment"] = env
        elif app_name == "tui":
            # The installed-product journey drives what the archive's
            # `install.sh` put in its prefix, not the build-tree binary —
            # `_installed_tui_app_override` explains why, and raises rather
            # than silently falling back (the linux override's rule).
            cli_bin = _installed_tui_app_override(request) or _resolve_cli_binary()
            if cli_bin:
                config["app_path"] = str(cli_bin)
            # Same shared-Rust receive-loop cadence knob as linux above: the tui
            # wires the in-process `ConversationsSession` at login, and the
            # real-wire round-trips need the fast drain tick, not the 30 s
            # production default.
            #
            # FAUNA_DNS_PROVIDER_FAKE wires the e2e fake DNS-provider decorator,
            # the same sentinel-gated hook linux/windows/macos/ios take above, so
            # the tier_2 managed-publish harness (test_admin_dns_managed.py) can
            # drive verify()/publish() with no real DNS provider. tui builds the
            # credentialed machine in-process at `admin::init`
            # (`build_dns_management_machine_with_credentials`, which reads this
            # var), and the tui driver merges `config["environment"]` into the
            # launch env (`drivers/tui.py`), so nothing else is needed. Production
            # never sets it, and the decorator is transparent for non-sentinel
            # credentials.
            config["environment"] = {
                "FAUNA_CONV_POLL_SECS": "2",
                "FAUNA_DNS_PROVIDER_FAKE": "1",
            }
            # The share plane's knobs — tui leads the plane. These lived inline
            # here until 2026-08-21; they are shared now because the journey is
            # app-parametrized and the env has to be too.
            _apply_share_plane_env(config["environment"], app_name)
            _apply_place_accepts_log_env(config["environment"])
            # The actuation gate (convention 11 one layer down), same flags as
            # apple and linux. tui is NOT staging: it has refused a disabled
            # `click` since its automation surface existed, and as of 2026-08-21
            # all five drive routes go through the shared
            # `fauna_e2e_agent::gate_actuation` with `default_strict = true`. So
            # `--permissive-actuation` is the way OUT of refusal here (apple's
            # posture), and `--actuation-log PATH` harvests the markers a
            # permissive sweep leaves. Until this call existed, neither flag
            # reached the tui launch at all — the app read its default and a
            # `--app tui` sweep silently measured nothing.
            _apply_actuation_mode_env(config["environment"], request)
            _apply_r14_trust_env(config["environment"], nest_instance, request)
            _apply_mail_import_source_trust_env(config["environment"], request)
            _apply_release_feed_env(config["environment"], request)
        elif app_name == "android":
            config["app_path"] = str(_repo_root / APP_PATHS["android"])
            # The bridge APK and the run's device (`--device-serial` /
            # `E2E_DEVICE_SERIAL`), from the one home the launch harness reads
            # too (`helpers/android_device.run_launch_facts`). The driver has
            # always accepted `device_serial` and passes `-s`; None means "the
            # one attached device".
            from helpers import android_device

            config.update(android_device.run_launch_facts(_repo_root))
            # Real-conversations launch gate (the `real_conversations` marker):
            # forwarded over the bridge's /session POST as an
            # FAUNA_E2E_REAL_CONVERSATIONS intent extra -- android's twin of the
            # windows/macOS/iOS launch-time gate (see the helper's docstring).
            # NOTE: unlike windows/macOS (real subprocesses that inherit actual
            # OS env vars), an android app is launched via Intent, not exec, so
            # there is no such thing as setting its process environment from
            # outside -- only keys BridgeHttpServer.kt's launchApp() explicitly
            # reads off this dict ever reach the app. FAUNA_E2E_TRUST_NEST_IDENTITY
            # is now one of them: launchApp() forwards it
            # as an intent extra, and MainActivity.onCreate re-exports it into the
            # real process env via android.system.Os.setenv before any auto-login
            # can assemble the account runtime -- see _apply_r14_trust_env's
            # docstring.
            # _apply_real_conversations_env also sets FAUNA_CONV_POLL_SECS,
            # which is NOT forwarded to android yet (the shared Rust
            # start_receive_loop reads it via std::env::var, which cannot be
            # set for an already-launched android process this way) -- the e2e
            # receive loop still works, just on the 30s production poll
            # instead of the 2s test cadence. Untested empirically (no android
            # e2e has ever run against a real device); verify on the emulator host.
            env = {}
            _apply_real_conversations_env(env, request)
            _apply_r14_trust_env(env, nest_instance, request)
            _apply_mail_import_source_trust_env(env, request)
            _apply_release_feed_env(env, request)
            config["environment"] = env
            # Nest ports the driver opens an `adb reverse` for, so the device's
            # own 127.0.0.1:<port> IS the nest (testing.md § Default app and nest
            # mode → Android's run venue, constraint 3). `url`'s port is read by
            # the driver itself; this list is for the ADDITIONAL nests of a
            # multi-nest run that are known at launch time. A nest started after
            # the app launched takes `AndroidBridgeDriver.ensure_reverse`.
            config["nest_ports"] = [
                p for p in (nest_instance.get("port"),) if p is not None
            ]
        return config


@pytest.fixture(scope="session")
def _driver_cache(request, nest_mode):
    """Session-scoped cache: one driver per client, launched once, reused everywhere.

    All per-test fixtures (app, logged_in_app, fresh_app, admin_app) call
    driver.reset() or driver.set_state() instead of launching a new browser/bridge.

    ``nest_mode`` is declared here and used only for its id stamp: every app
    launched through this cache points at whatever nest the mode selected, so a
    plain-``app`` test IS on the nest axis even though the lazy resolution below
    keeps ``nest_instance`` out of its declared closure. Declaring the mode (a
    pure config read, no nest start) is what makes those tests stamp correctly
    without re-eagering the nest — see ``pytest_generate_tests``.

    The local nest is resolved LAZILY, not declared: a `live_nest` session
    (every selected test signs into a live remote nest — see
    `_live_nest_session`) must not pay the full local `fauna-nest` build for a
    nest it never touches, so in that case clients launch pointed at the live
    URL instead. Never re-add `nest_instance`/`test_user` as eager params —
    that silently reverts the fix for every live session (pinned by
    `test_live_nest_session_gate.py`).
    """
    if _live_nest_session(request):
        from helpers import multiseat_config

        live = True
        # Only ["url"] is read by `_build_app_config`; the client discovers
        # the actual nest by DoH from the handle at sign-in, so the launch URL
        # is advisory (multiseat_config.nest_url: FAUNA_LIVE_NEST_URL, default
        # the shared live box).
        nest_target: dict = {"url": multiseat_config.nest_url()}
    else:
        live = False
        # Resolved during THIS fixture's setup (not inside get_driver) so
        # creation/teardown ordering against the nest is identical to the
        # former eager params: the nest outlives the drivers.
        request.getfixturevalue("test_user")
        nest_target = request.getfixturevalue("nest_instance")
    cache: dict[str, object] = {}

    def get_driver(app_name: str):
        # The second of the catalog's two driver doors. `create_driver` covers a
        # fresh launch; this covers the far commoner case of a session-cached driver
        # being handed to a later test, which never constructs anything and would
        # otherwise leave every test after the first unattributed.
        from helpers import feature_ledger
        feature_ledger.note_app(app_name)
        if app_name in cache:
            return cache[app_name]
        if live and app_name == "web":
            pytest.fail(
                "live_nest sessions cannot drive the web client: the SPA "
                "proxy (`spa_url`) targets the session's LOCAL nest, which a "
                "live_nest session deliberately never builds. Use a native "
                "client, or drop the live_nest marker."
            )
        config = _build_app_config(app_name, nest_target, request)
        driver = create_driver(app_name)
        # Apple is driven in-process (no XCUITest / AutomationMode) like every
        # other app, so launch is a plain call — the AutomationMode-wedged /
        # single-runner-busy abort paths were retired with the XCUITest bridge.
        driver.launch(config)
        # Render-readiness preflight for the apple in-process drivers. The app can
        # launch fully "healthy" (process up, /health 200, /app/state correct) yet
        # create NO window when the host GUI session is degraded — then no SwiftUI
        # body evaluates, the automation registry is empty, and every element query
        # times out, producing a ~45-min all-red run that looks like an app
        # regression. Fail the whole session fast + legibly instead. (Replaces the
        # render-coverage half of the AutomationMode gate the in-process cutover
        # removed; the fix is environmental — reboot/GUI re-login, not code.)
        from drivers.inprocess_agent import InProcessAgentDriver, RenderNotReady
        if isinstance(driver, InProcessAgentDriver):
            try:
                driver.assert_render_ready()
            except RenderNotReady as exc:
                try:
                    driver.teardown()
                except Exception:
                    pass
                pytest.exit(
                    f"apple in-process render preflight FAILED for '{app_name}': {exc}",
                    returncode=3,
                )
        try:
            driver.wait_for_state(lambda s: s is not None, timeout=30)
        except (TimeoutError, Exception):
            pass
        cache[app_name] = driver
        return driver

    yield get_driver

    for app_name, driver in cache.items():
        try:
            driver.screenshot(f"teardown-session-{app_name}")
        except Exception:
            pass
        driver.teardown()


@pytest.fixture(scope="session")
def web_api_url(request):
    """The URL the web SPA should use for API calls.

    For web apps this is the SPA proxy URL (which forwards /api/ to nest).
    For native apps this is the raw nest URL. Tests that inject session
    state via set_state() should use this instead of nest_instance["url"]
    when the client is web.

    Native-only runs (e.g. ``--client linux``) must NOT touch ``spa_url``: it
    depends on ``static_dir``, which ``pytest.skip``s when the web app
    isn't in the active set. That ``Skipped`` would propagate through here and
    silently skip every test/fixture that depends on ``web_api_url`` — which
    includes ``test_navigation.py``'s page-load suite and
    ``test_sp_onboarding.py::test_login_via_state`` (and the latter is the
    precondition for ``test_authenticated_nav_after_login``). Gate on the
    active client set so native runs go straight to the raw nest URL.
    """
    if "web" in get_available_apps():
        return request.getfixturevalue("spa_url")
    return request.getfixturevalue("nest_instance")["url"]


@pytest.fixture(scope="session", params=get_available_apps())
def persistent_app(request, _driver_cache):
    """A single app instance for the entire test session.

    Uses the session-scoped driver cache. Tests use driver.set_state()
    to set up preconditions instead of reinstalling.
    """
    app_name = request.param
    driver = _driver_cache(app_name)
    layer = ActionLayer(driver)
    yield layer


@pytest.fixture(params=get_available_apps())
def app(request, _driver_cache):
    """Yields a reset ActionLayer for each available client.

    Reuses the session-scoped driver and calls reset() between tests
    instead of launching a fresh browser/bridge each time.
    """
    from drivers.http_bridge import BridgeDead

    app_name = request.param
    driver = _driver_cache(app_name)

    # Module-boundary cold relaunch — a 7-app fixture contract, driver-type-
    # free (testing.md § point 10; helpers/module_relaunch.py owns the story).
    # A session-scoped process accumulates state reset() does not model, so
    # each module starts against a freshly relaunched app — the baseline it
    # would see run in isolation. The session's first module pays nothing
    # (_driver_cache just launched fresh), and the reset probe below stays the
    # standing detector for reset()-survivable state — the relaunch is not its
    # replacement.
    from helpers import module_relaunch

    outcome = module_relaunch.at_module_boundary(
        driver, request.node.module.__name__ if request.node.module else None
    )
    if outcome is not None:
        print(f"[e2e] {app_name}: {outcome}")

    try:
        driver.reset()
    except (BridgeDead, TimeoutError) as exc:
        # Either the bridge died or the previous test left the app's UI
        # thread stuck (e.g. an unclosed modal dialog), so `reset()` can't
        # complete its Navigate callback. Drivers that can relaunch the app
        # (Windows via FlaUI bridge) clear the state and give the next test
        # a fresh app; the bridge itself stays alive. Skip reasons carry the
        # underlying exception: a skip shows no captured stderr, so without
        # this the -rs summary is the only diagnostic anyone gets.
        if not driver.recover():
            pytest.skip(
                f"Bridge for {app_name} died during an earlier test and "
                f"could not recover (reset: {type(exc).__name__}: {exc})"
            )
        try:
            driver.reset()
        except (BridgeDead, TimeoutError) as exc2:
            pytest.skip(
                f"Bridge for {app_name} recovered but reset still failed "
                f"({type(exc2).__name__}: {exc2})"
            )
    _post_reset_surface_probe(driver, request)
    layer = ActionLayer(driver)
    yield layer
    # The convention-17 layer-(b) probe used to fire HERE, and reached only the
    # tests that take *this* fixture — 7 modules define their own `app` over
    # `persistent_app` and shadowed it entirely. It rides
    # `pytest_runtest_teardown` now, before any fixture finalizer runs.
    try:
        driver.screenshot(f"post-test-{app_name}-{request.node.name}")
    except Exception:
        pass


# The surface a healthy reset MUST land on (`App::reset` -> `LaunchSurface::Wizard`
# + `OnboardingStep::IdentityChoice`, which is `#[default]`).
_RESET_PROBE_EXPECTED = "create-identity-button"

# Discriminating markers for every OTHER surface a poisoned app could be showing
# instead. Only probed when the expected marker is ABSENT, so the healthy path
# costs a single round-trip.
#
# The NON-WIZARD `LaunchSurface` arms (launch.rs) carry most of the probability
# mass, not the wizard steps: `reset()` forces `LaunchSurface::Wizard` and
# `wizard.reset()` forces `IdentityChoice` (app.rs:1603, 1693), AND the driver's
# own reset polls until `session.authenticated` is false (http_bridge.py:733-737)
# — so a poisoned app is signed out and somewhere OTHER than the wizard, which is
# exactly this family. Omitting them was the first version's blind spot.
_RESET_PROBE_SURFACES = (
    # Non-wizard launch surfaces (launch.rs:155-246).
    ("launch-transient-retry", "launch-transient-error"),
    ("launch-instance-chooser", "launch-instance-chooser"),
    ("launch-identity-changed", "nest-identity-changed-warning"),
    # Covers TransientRetry/NeedsUpdate/InstanceChooser — the widest single net.
    ("launch-fallthrough", "launch-fallthrough-button"),
    ("unlock-or-create-passphrase", "tui-unlock-passphrase-input"),
    # Wizard steps other than IdentityChoice.
    ("wizard-handle-entry", "handle-input"),
    ("wizard-identity-created", "identity-continue-button"),
    ("wizard-identity-import", "import-submit-button"),
    ("wizard-recovery-kit", "recovery-kit-confirm-button"),
    ("wizard-recovery-entry", "recovery-entry-submit-button"),
    ("wizard-claim-code", "claim-code-input"),
    ("wizard-nest-recovery", "recover-method-cloud-button"),
    ("wizard-dns-config", "dns-config-continue-button"),
    ("authenticated", "feed-tab"),
)


def _post_reset_surface_probe(driver, request):
    """Opt-in trace of which surface the app landed on after the fixture reset.

    Off unless ``FAUNA_E2E_RESET_PROBE`` names a file; costs nothing otherwise.

    Exists for the batch-vs-solo divergence class (testing.md § point 10): a
    test passes alone and fails in a full run because an EARLIER test left the
    session-scoped app process somewhere the fixture ``reset()`` above does not
    undo. Bisecting for the poisoner costs O(log n) full runs; this costs one,
    because the trace names the exact test after which the surface changes.

    Writes to a FILE, not stdout, for two reasons: pytest swallows fixture
    prints on passing tests (and the poisoner itself passes — it is the tests
    AFTER it that fail), and a long run reaped mid-flight still leaves the
    trace on disk, where pytest's own end-of-run ``FAILURES`` section would
    have been lost with the process.
    """
    path = os.environ.get("FAUNA_E2E_RESET_PROBE")
    if not path:
        return

    def count(element_id):
        try:
            return driver.count(element_id)
        except Exception as exc:  # a probe must never fail the run it observes
            return f"?{type(exc).__name__}"

    expected = count(_RESET_PROBE_EXPECTED)
    if expected == 1:
        verdict = "OK"
    else:
        # Only here do we pay for the wider sweep — this is the interesting case.
        found = [lbl for lbl, eid in _RESET_PROBE_SURFACES if count(eid) == 1]
        # An all-zeros sweep is itself a signature, not a dead end: `LaunchSurface::
        # Launching` is "Chrome only — no ui.yaml element" (launch.rs:48), so a LIVE
        # agent reporting zero everywhere means the app is mid-hydration/silent
        # challenge. (A WEDGED agent raises instead, and shows up as `?Exception`.)
        where = ",".join(found) or "no-known-surface(launching-or-unmapped)"
        # The error line carries the refused-agent-command banner and the
        # NeedsUpdate message — often the only text naming WHY the app is here.
        try:
            state = driver.get_state() or {}
            err = (state.get("messages") or {}).get("error") or ""
        except Exception as exc:
            err = f"?{type(exc).__name__}"
        verdict = f"POISONED[{_RESET_PROBE_EXPECTED}={expected}] -> {where}"
        if err:
            verdict += f" err={err!r}"
    try:
        with open(path, "a") as fh:
            fh.write(f"{verdict}\t{request.node.nodeid}\n")
            fh.flush()
    except OSError:
        pass


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_makereport(item, call):
    """Stash each phase's outcome on the item, and attach the app's own log to a failure.

    The probe runs in fixture teardown, where the test's own verdict is not
    otherwise visible. It needs it because a violated invariant on a frame left
    by an ALREADY-FAILING test is usually a consequence of that failure, not a
    finding — and a triage pass that cannot separate the two drowns in the noise
    of whatever broke first.

    The second job is convention 6's, applied once instead of per test: a failing
    test gets the client's own log tail in its report (`helpers/app_log_section.py`).
    ⚠ **This position is the mechanism, not a convenience.** It runs after the call
    phase and *before* the teardown that reclaims a per-instance data dir — windows'
    e2e data dirs are `mkdtemp` and its teardown `rmtree`s them, so a reader wired
    anywhere later finds nothing. That is exactly how a `--app windows` succession
    run came back unable to say why two aftermath legs did nothing.
    """
    outcome = yield
    report = outcome.get_result()
    setattr(item, f"_e2e_rep_{call.when}", report)
    if call.when == "call" and report.failed:
        _attach_app_log_section(item, report)


def _attach_app_log_section(item, report):
    """Put the driver's own log tail on a failing test's report.

    Resolved from `item.funcargs` through the same `driver_for` the frame probe
    uses — by fixture VALUE, so it rides every driver-bearing test whatever built
    the driver, including the seven modules that shadow the conftest `app`. Then
    one labelled section per seat a launcher fixture started in the test body
    (`app_log_section.launched_drivers`), which no fixture value names directly.

    Every failure mode here is swallowed: a diagnostic that can break the run it
    was only meant to explain gets switched off and never switched back on.
    """
    try:
        from helpers import app_log_section, frame_invariants

        funcargs = getattr(item, "funcargs", None) or {}
        driver = frame_invariants.driver_for(funcargs)
        section = app_log_section.build(driver)
        if section is not None:
            report.sections.append(section)
        for label, seat in app_log_section.launched_drivers(funcargs):
            if seat is driver:
                continue
            section = app_log_section.build(seat, label=label)
            if section is not None:
                report.sections.append(section)
    except Exception:  # pragma: no cover - diagnostics must never mask a failure
        pass


# This run's frame tally, reported by `pytest_terminal_summary`. Module-level
# because the probe fires in fixture teardown and the reporter runs at session
# end, and a pytest run is one process.
_frame_tally = None


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_teardown(item, nextitem):
    """Fire the layer-(b) probe on the test's last frame, before anything unwinds.

    This hook rather than a fixture's own teardown, because the fixture the
    probe hung off reached far less of the suite than it looked like: seven
    modules define their own `app` over `persistent_app` and shadow the conftest
    one outright — `test_navigation.py` among them, whose 9 green tests produced
    not one observation. `item.funcargs` names every fixture the test ACTUALLY
    resolved, so resolving the driver from there rides every driver-bearing
    test whatever built its driver.

    ⚠ `tryfirst` + the pre-`yield` position are load-bearing: the wrapped
    implementation is what runs the fixture finalizers, and the frame must be
    read while it is still the frame the test left.
    """
    from helpers import frame_invariants

    driver = frame_invariants.driver_for(getattr(item, "funcargs", None) or {})
    if driver is not None:
        _post_frame_invariant_probe(driver, item)
        _record_foreground_takes(driver, item)
    yield


# This run's windows foreground-take tally (convention 10's windows focus axis),
# reported by `pytest_terminal_summary` — module-level for the same reason as
# `_frame_tally` below.
_foreground_tally = None


def _record_foreground_takes(driver, node):
    """Attribute the bridge's foreground-taking gestures to the test that caused
    them (`helpers/foreground_takes.py`). windows-only by construction — only its
    driver answers `foreground_report` — and observation only, never a failure."""
    global _foreground_tally
    from helpers import foreground_takes

    drained = foreground_takes.drain(driver)
    if drained is None:
        return
    path = Path(__file__).parent / foreground_takes.CORPUS_NAME
    if _foreground_tally is None:
        _foreground_tally = (foreground_takes.RunTally(), path)
    _foreground_tally[0].record(node.nodeid, drained)
    if not drained.entries:
        return
    try:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(foreground_takes.format_lines(node.nodeid, drained))
    except OSError:
        pass


def _post_frame_invariant_probe(driver, node):
    """Convention 17 layer (b): check general invariants on each test's LAST frame.

    **Observes by default** (2026-08-15): ``FAUNA_E2E_FRAME_INVARIANTS`` unset
    appends to the harness's own gitignored corpus, a path redirects it, and an
    off-token (``0``/``off``/``no``/``false``/empty) switches it off — the
    resolution, and why the default flipped, live in
    ``helpers/frame_invariants.py``. One state read per frame; the catalogue and
    the bug class each entry catches live there too.

    Every failure mode here is swallowed on purpose. A checker that can break a
    run it was only supposed to watch is worse than no checker: it would be
    switched off across the fleet after its first bad day and never switched
    back on. That is exactly what makes observing by default safe.
    """
    global _frame_tally
    from helpers import frame_invariants

    path = frame_invariants.resolve_corpus_path(
        os.environ.get("FAUNA_E2E_FRAME_INVARIANTS"), Path(__file__).parent
    )
    if path is None:
        return

    try:
        state = driver.get_state()
    except Exception as exc:
        state = None
        read_error = f"{type(exc).__name__}: {exc}"
    else:
        read_error = None

    violations, not_applicable = frame_invariants.evaluate(state)
    report = getattr(node, "_e2e_rep_call", None)
    outcome = getattr(report, "outcome", None)
    if _frame_tally is None:
        _frame_tally = (frame_invariants.RunTally(), path)
    _frame_tally[0].record(node.nodeid, violations, read_error)
    try:
        with open(path, "a") as fh:
            fh.write(
                frame_invariants.format_line(
                    node.nodeid,
                    violations,
                    not_applicable,
                    outcome,
                    unobserved=read_error,
                )
            )
            fh.flush()
    except OSError:
        pass

    if violations and os.environ.get("FAUNA_E2E_FRAME_INVARIANTS_STRICT"):
        # The promoted stage. Deliberately not the default: convention 17's
        # rollout is observe-and-log first, promote after a clean sweep.
        pytest.fail(
            "post-frame invariant violation(s) on the last frame of this test:\n  "
            + "\n  ".join(violations)
        )


@pytest.fixture
def logged_in_app(request, app, nest_instance, test_user):
    """An app fixture that is already logged in.

    Uses set_state() for drivers that support it (bridge-backed drivers with
    a test agent). Falls back to UI-based login for others.

    For web apps, node_url must be the SPA proxy URL (not the raw nest URL)
    because the browser makes fetch() calls relative to it, and the proxy
    forwards /api/ requests to the nest.
    """
    _login_app_as(app, request, nest_instance, test_user)
    return app


@pytest.fixture
def calendar_backend(logged_in_app):
    """Mint the actor's MSEK, so the encrypted CalDAV store is usable.

    The flipped Events pages read/write the encrypted CalDAV store
    (`bridge_caldav_*`) via `fauna-client-caldav` (events.md Decision B), which
    needs the actor's MSEK — minted by enabling mail (`MailSettingsMachine::
    enable_mail`). **Without it the page degrades to an empty "enable calendar"
    state and `create_calendar`/`create_event` silently no-op** — the calendar
    list simply stays at zero rows with no error, which reads downstream as a
    broken sidebar rather than a missing precondition (it cost a full 35-minute
    e2e cycle to diagnose exactly that way, 2026-08-02).

    Gated to the apps exercising the encrypted path in e2e: **linux + web +
    windows + macOS + iOS + tui**. android app code is also flipped
    (events.md § Implementation status), but enabling its e2e here is a per-app
    follow-up owned by that app's sessions. Idempotent across the
    session-scoped nest, so the cost is paid once.

    Lives here rather than in one test module because it is a **precondition of
    the calendar surface**, not of one file's tests: `test_events.py` and
    `test_calendar_visibility.py` both need it, and a second copy would be the
    per-app-divergence mistake one directory down (priority #2/#4). A module
    that needs it on every test wraps it in its own `autouse` fixture.

    Re-requesting is also how a test **re-hydrates after a client relaunch**: a
    fresh data dir holds no cached mail key, so the app must re-fetch the mail
    config (incl. the wrapped MSEK) from the server before it can open the
    sealed store again — call `ensure_mail_enabled()` again after re-login, the
    same idempotent confirm `test_mail_sent_copy_restart.py` makes.
    """
    d = logged_in_app.driver
    if (
        d.is_linux()
        or d.is_web()
        or d.is_windows()
        or d.is_macos()
        or d.is_ios()
        or d.is_tui()
    ):
        logged_in_app.mail_settings.navigate()
        logged_in_app.mail_settings.ensure_mail_enabled()
    return logged_in_app


@pytest.fixture
def ungranted_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED fresh actor on the shared session nest —
    an owner who has granted **nothing**, and whom no other test can change.

    Use this for any assertion whose precondition is *"this owner holds no
    grants"* — the trust facet's ``nest-trust-empty`` state, an unenrolled
    Backups page, an empty capability list. Those projections are **owner-scoped,
    not nest-scoped** (`backup_handlers.rs` ``backup_status_handler`` opens the
    coordinator ``for_owner(actor_id)``; the content-grant facet folds the
    *client-authoritative* grant log out of this actor's own config), so a fresh
    actor on the shared nest is empty by construction — no extra nest boot, and
    web keeps the default ``spa_url`` proxy because the nest is the same one.

    **Why a fixture and not cleanup discipline.** The shared session ``test_user``
    accumulates state for the whole run, and some of it is *deliberately durable*:
    enrolling a backup destination grants that owner's ``NestBackupKey`` to the
    nest, and removing the destination pointedly does **not** revoke it
    (``backup_enroll.rs`` § deregister — freezing the seal is a distinct
    trust-facet action a user must choose). So the seal row outlives every
    destination test, and `test_nest_trust.py`'s empty-state assertions failed the
    moment they ran after `test_backups.py` in one command. Asking each enrolling
    test to un-enroll would make those assertions depend on the teardown
    discipline of every present *and future* test in a 200-test session — an
    invariant no later session can see, let alone check. Establishing the
    precondition here instead makes it locally verifiable and unbreakable.

    Same trade, same reason as ``seeded_media_app`` (dedicated actor so its seed
    cannot pollute an empty-state test logged in as ``test_user``).
    """
    # `verify_live_actor=True` by the barrier's own stated rule: it is **on for a
    # fixture whose whole premise is a DEDICATED actor**, and this fixture's whole
    # premise is a dedicated fresh actor. Without it a silently-declined switch
    # leaves the app running as the previous test's actor and every owner-scoped
    # read below comes back correct-but-empty *for that actor* — which is not a
    # weaker answer, it is the wrong one, and it reads as a broken feature rather
    # than a dropped login (e2e-conventions.md convention 11). Measured on
    # `test_identity_succession_ceremony.py --app macos`, where the succession
    # test really switches accounts and the two tests after it inherited its
    # successor.
    _login_app_as(app, request, nest_instance, _make_user(nest_instance),
                  verify_live_actor=True)
    return app


@pytest.fixture
def succeedable_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED fresh actor, plus that actor's own
    credentials — for tests that must act on the identity *from outside the app*.

    Returns ``(app, user)``, where ``user`` carries ``actor_id_hex`` and
    ``signing_key`` (the identity seed). ``ungranted_app`` returns only the app,
    so a test that needs to name the logged-in identity to a nest-side ceremony
    cannot use it.

    DEDICATED, never the shared session ``test_user``, for a stronger reason
    than the usual pollution one: the intended ceremony is **identity
    succession**, which re-points the account and revokes every session of the
    old identity. Running that against the shared user would break every later
    test in the run.
    """
    user = _make_user(nest_instance)
    # `verify_live_actor=True` by the barrier's own rule, as ``ungranted_app``
    # does: this fixture's whole premise is a dedicated actor, and the journeys
    # that take it run AFTER successions in the same module. A declined switch
    # would leave the app running as an earlier, possibly superseded actor, and
    # the journey would then fail on some downstream precondition rather than
    # on the dropped login it actually is.
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return app, user


@pytest.fixture
def dedicated_actor_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED fresh actor, plus that actor — for a
    journey that leaves its actor DAMAGED for the rest of the run, by design.

    Returns ``(app, user)`` like ``succeedable_app``. The damage this exists for
    is a mail-key teardown. Turning mail off email-only clears the MSEK and its
    grace generations (``mail-credentials.md`` § MSEK lifecycle), so every record
    already in the mailbox can never open again. Every later module's cold
    relaunch re-drains the mailbox and counts those records again, which keeps
    the unopenable-mail floor up on the conversations page for the rest of the
    run (``ui/conversations.md`` § Errors & edge cases → *A fifth truth*); no
    gesture clears it. On the shared ``test_user`` that failed every later
    "page shows no error" assertion in a whole-suite sweep (e2e convention 10:
    a test must not poison the shared identity).
    """
    user = _make_user(nest_instance)
    # `verify_live_actor=True` by the barrier's own rule: the whole premise is a
    # dedicated actor, and a silently-declined switch would run the journey as
    # the previous test's actor — typically the shared one this keeps clean.
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return app, user


@pytest.fixture(scope="module")
def self_signed_nest(request, nest_mode, tmp_path_factory):
    """A claimed nest serving its always-live SELF-SIGNED floor cert over REAL
    HTTPS on its API listener — the only nest fixture that does.

    Every other tier_3 nest rides the process-wide ``FAUNA_INSECURE_DISABLE_TLS``
    escape (``pytest_sessionstart``) and serves plain HTTP, so no ordinary e2e
    ever walks a client's TLS-trust path. This one drops the escape (``serve_tls``),
    which makes it the harness for the self-signed-floor trust posture:
    ``security.md`` § Transport trust — a client accepts the nest's cert iff it is
    WebPKI-valid, OR the host is loopback, OR its SPKI matches the channel-binding
    pin. Bound to ``127.0.0.1``, so a client reaches it on the **loopback** branch
    — the same-box-install case (``installers/windows.md`` § Implementation status
    today → *Same-box app default URL* ``https://127.0.0.1:443``).

    A user is registered, so this is a drop-in TLS twin of ``nest_instance`` +
    ``test_user`` for any client (not windows-specific).
    """
    # A browser dials this nest RAW (the create journey's handle check and
    # sign-in), so it must allow this run's SPA origin — empty on a run with no
    # web app (`browser_origins_to_allow`).
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "self-signed-nest", serve_tls=True,
        cors_origins=browser_origins_to_allow(request),
    )
    # True in both modes for different reasons — standalone dropped the
    # process-wide plain-HTTP escape for this nest, docker's image has no
    # plain-HTTP posture to drop — and this fixture's whole premise is that a
    # client walks a REAL TLS path, so it is worth failing loudly either way.
    assert nest["url"].startswith("https://"), (
        f"serve_tls nest must report an https url, got {nest['url']!r}"
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def rotatable_tls_nest(request, nest_mode, tmp_path_factory):
    """``self_signed_nest``'s TLS posture on ``rotatable_nest``'s lifecycle: a
    function-scoped, claimed, user-registered nest serving its self-signed
    floor cert over real HTTPS, whose deployment identity a test may rotate.

    Both properties are load-bearing for the rotation-acceptance journey
    (``test_nest_rotation_repin.py``): a client only TOFU-pins over TLS (so the
    plain-HTTP ``rotatable_nest`` can never produce the pin the chain must
    move), and rotation is deployment-destructive (so the session-scoped
    ``self_signed_nest`` must never be rotated under later tests).
    """
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "rotatable-tls", serve_tls=True,
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def self_signed_logged_in_app(request, app, self_signed_nest):
    """``app``, logged in against the self-signed-HTTPS nest instead of the
    shared plain-HTTP ``nest_instance``.

    Every subsequent client→nest call in the test therefore rides real TLS
    against a cert no public CA signed. On windows that is what puts the C#
    ``DirectNestClient`` residual-HTTP legs (health / blob) through
    ``ValidateServerCertificate`` — the .NET ``HttpClient`` the Rust rustls pin
    store cannot govern (``security.md`` § Transport trust, the C# leg sub-bullet).

    web declares the whole leg absent FIRST — ``_declare_no_web_cert_trust``,
    before the login attempt, because a declaration placed after the thing it
    declares absent never fires.
    """
    _declare_no_web_cert_trust(app)
    _login_app_as(app, request, self_signed_nest, self_signed_nest["user"])
    return app


def _declare_no_web_cert_trust(app) -> None:
    """web declares no client-side cert-trust decision — **at fixture level,
    before the login**, which is the whole point of this helper existing.

    The two ``*_logged_in_app`` fixtures below exist to put a client's *own* trust
    policy on a real self-signed floor: ``webPkiValid || isLoopback || spki == pin``
    (``security.md`` § Transport trust). web never evaluates that policy. The
    **browser** terminates TLS and hands WASM no certificate, so there is no SPKI to
    compare and no branch to take — ``security.md`` § Transport trust says so in its
    own words (*"a browser exposes no received certificate to WASM, so there is no
    SPKI to compare the signature against and Axis 1's channel binding cannot be
    completed"*, and it is *"structural, not a gap to close later"*; the pre-claim
    row adds *"web cannot reach a self-signed nest anyway"*).

    ⚠ **Both test files declared this already — in their test BODIES**, after
    ``self_signed_logged_in_app``/``spki_pinned_logged_in_app`` had already run. So
    on web the fixture logged in first and the declaration never got the chance to
    fire: all four legs ERRORed at setup in ``ConnectionBarrierTimeout`` instead of
    skipping. A declaration that runs
    after the thing it is declaring absent is not a declaration.

    ⚠ **And the reason those bodies gave was false**, which is why this text does not
    reuse it. It said a browser "cannot be told to trust a self-signed cert"; a
    browser *can* — ``web-bridge/server.py``'s ``_CONTEXT_KWARGS`` sets Playwright's
    ``ignore_https_errors`` on every context precisely so a ``wss://`` to a
    self-signed foreign nest works, and that is recorded there as measured. What web
    lacks is not the ability to *reach* such a nest; it is the ability to *decide* to,
    which is the only thing these legs assert. Nor was the harness ever going to
    demonstrate it: web reaches a nest through ``_serve_spa_proxy``, and it is the
    PROXY, not the browser, that would have to trust the floor cert — so a green web
    leg here would witness the harness's own trust decision, not the product's.
    Vacuous either way. (That proxy cannot in fact carry it: its WS splice speaks
    plaintext into the TLS listener — see ``_serve_spa_proxy``'s own warning.)

    ⚠ **Called ONLY by the two dedicated cert-trust fixtures, never by the shared
    login funnel.** Between 2026-09-01 and 2026-09-09 ``_login_app_as`` called this
    for every web login against an ``https://`` nest, on the premise that the SPA
    proxy could not bridge TLS at all — which made every web app-driven journey
    under ``--nest docker`` a declared absence, and made the declaration say
    something false: the property declared absent here is the client's *trust
    decision*, and an ordinary journey against a TLS nest asserts nothing about
    trust. The proxy's WS hop now carries the run's floor-cert posture
    (``_serve_spa_proxy``, ``common.auth.upstream_tls_context``), so web runs its
    journeys in docker mode like every other app and this helper is back to the
    one job it had: the two legs whose SUBJECT is the trust decision.
    """
    if app.driver.is_web():
        from helpers.app_surface import declared_absence

        declared_absence(
            app.driver,
            capability="client-side self-signed-floor cert trust (webPkiValid || "
            "isLoopback || spki == pin) — the browser terminates TLS and exposes no "
            "received certificate to WASM, so the client evaluates no branch",
            doc="architecture/security.md § Transport trust — 'a browser exposes no "
            "received certificate to WASM, so there is no SPKI to compare the "
            "signature against and Axis 1's channel binding cannot be completed'; "
            "'the difference is structural, not a gap to close later'",
        )


@pytest.fixture(scope="module")
def atproto_hosted_nest(request, nest_mode, tmp_path_factory):
    """A claimed nest whose ``handle_domain`` is a **public** DNS name, so the
    AT Protocol depth selector's hosted rungs are ENABLED
    (``resolve_handle_domain(domain).is_public_dns_name`` — ``fauna.test`` is
    not localhost/`.local`/an IP literal, so it passes the pure syntactic gate).

    The shared ``nest_instance`` is domainless (→ ``localhost`` → gated), which
    is exactly where the selector's gate-greying is tested; this is its twin for
    the hosted ladder + the full-PDS panel. Reaching a hosted level over the
    wire needs no real PLC directory: ``set_integration_level`` writes the mint
    *intent* + the level and returns — the DID is minted asynchronously by the
    bridge (absent here), so the identity simply reads as ``pending``.

    A user is registered, so this is a drop-in hosted-capable twin of
    ``nest_instance`` + ``test_user`` for any client.
    """
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "atproto-hosted-nest",
        claim_domain="fauna.test",
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def atproto_hosted_logged_in_app(request, app, atproto_hosted_nest):
    """``app``, logged in against the public-``handle_domain`` nest instead of
    the shared domainless ``nest_instance`` — so the Bluesky selector can enter
    hosted levels and reveal the full-PDS panel. Overrides ``spa_url_fixture``
    to ``atproto_hosted_spa_url`` — this nest is dedicated (module-scoped, not
    ``nest_instance``), and `_login_app_as`'s own docstring is explicit that a
    dedicated nest MUST pass its own proxy fixture for web.

    Every journey on this nest writes an account-plane kind — the hosted
    entry's senior rotation key (`fauna.state.atproto-identity`), a mint's
    secret (`fauna.state.atproto`) — so the fixture waits for the account
    runtime's assembly, a task spawned off the login path with no other
    completion signal (on web it follows the conversations manager's build),
    rather than let a write's own bounded wait run into a test's deadline. An
    app that publishes no account-store role key offers no such barrier
    (convention 11)."""
    from helpers.waiting import account_pump_role, await_account_runtime_assembled

    _login_app_as(app, request, atproto_hosted_nest, atproto_hosted_nest["user"],
                  spa_url_fixture="atproto_hosted_spa_url")
    if account_pump_role(app.driver) is not None:
        await_account_runtime_assembled(app.driver)
    return app


@pytest.fixture(scope="module")
def atproto_localhost_nest(request, nest_mode, tmp_path_factory):
    """A claimed nest whose ``handle_domain`` stays the ``localhost`` placeholder
    (no domain option at all → ``AppState::handle_domain`` default), so the Bluesky
    depth selector's hosted rungs are GREYED
    (``resolve_handle_domain("localhost").is_public_dns_name`` is false).

    The shared ``nest_instance`` is NOT usable for the greying assertion: the
    autouse ``_session_primary_mail_domain`` fixture pins ``fauna.test`` on it as
    the primary mail domain, so ``handle_domain()`` resolves to that public name
    and ``hosted_allowed`` is true (hosted rungs enabled). This dedicated nest is
    never touched by that pin, so it is a genuinely domainless twin — the
    localhost counterpart to ``atproto_hosted_nest``."""
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "atproto-localhost-nest",
        claim_domain=None,
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def atproto_localhost_logged_in_app(request, app, atproto_localhost_nest):
    """``app``, logged in against the dedicated domainless nest — so the Bluesky
    selector's hosted rungs are greyed (the localhost gate), which the shared
    ``logged_in_app`` cannot exercise (its nest carries a pinned public mail
    domain; see ``atproto_localhost_nest``). Overrides ``spa_url_fixture`` to
    ``atproto_localhost_spa_url`` for the same reason as
    ``atproto_hosted_logged_in_app`` — this nest is dedicated, not
    ``nest_instance``."""
    _login_app_as(app, request, atproto_localhost_nest, atproto_localhost_nest["user"],
                  spa_url_fixture="atproto_localhost_spa_url")
    return app


@pytest.fixture
def delete_account_nest(request, nest_mode, tmp_path_factory):
    """A dedicated claimed nest with a freshly-registered actor, for a REAL
    (not just scheduled) account deletion.

    Must NOT be the shared session ``nest_instance``: the completion half of
    account deletion needs the ``/api/v1/test/pending_actions/run_due`` test
    hook (``pending_actions_test_hook.rs``) to fast-forward the 14-day cool-off,
    and that hook's ``list_all_pending_actions`` is nest-WIDE, not actor-scoped
    (``bins/fauna-nest/src/db/pending_actions.rs::list_all_pending_actions``:
    "List all pending actions across all actors"). Calling it against the
    shared session nest would force-execute every OTHER test's in-flight
    pending action too (e.g. a handle-change cooldown) — a dedicated nest makes
    "nothing else is pending" true by construction instead of by test ordering.
    """
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "delete-account-nest")
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def delete_account_app(request, app, delete_account_nest):
    """``app``, logged in as the dedicated ``delete_account_nest`` actor —
    the account this test is actually allowed to delete.

    Web reaches the dedicated nest through ``delete_account_spa_url`` (NOT the
    shared ``spa_url``, which proxies ``nest_instance``): without it the browser
    would talk to the shared nest, where this actor is unregistered."""
    _login_app_as(app, request, delete_account_nest, delete_account_nest["user"],
                  spa_url_fixture="delete_account_spa_url")
    return app


@pytest.fixture
def dedicated_no_mail_nest(request, nest_mode, tmp_path_factory):
    """A dedicated claimed nest with a freshly-registered actor guaranteed to
    hold no MSEK — for a precondition that needs "mail has never been
    enabled" and cannot tolerate the shared session actor.

    ``nest_instance``/``test_user`` are session-scoped, and at least 5 test
    files sorting alphabetically before ``test_folder_webdav_toggle.py``
    call ``ensure_mail_enabled()`` on that one shared actor
    (``test_admin_serving_indicator.py``, ``test_apple_track_a_diag.py``,
    ``test_automation_registry_lifecycle.py``, ``test_events.py``,
    ``test_factory_reset_calendar_reclaim.py``), so a "must run before any
    mail-enabling test" precondition on the shared actor cannot hold in a
    full-suite run regardless of in-module ordering. A dedicated actor makes
    the precondition true by construction instead of by collection order.
    """
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "no-mail-nest")
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture(scope="module")
def web_hosting_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest whose registration handle domain is a real DNS-shaped
    name, so per-user subdomain hosting resolves to something servable.

    The shared ``nest_instance`` runs with an EMPTY handle domain, which makes
    ``subdomain_url(handle, url_host(node_url))`` compose
    ``https://alice.127.0.0.1:PORT/`` — syntactically a URL, but a host the
    serving layer's subdomain resolver can never match (it strips a
    ``.<nest_domain>`` suffix). The published-post management surface's copy
    affordances hand out exactly that composition, so proving the copied link is
    a link that *serves* needs a nest with a handle domain
    (``web-content-hosting.md`` § Published-post management, the e2e contract;
    same reason ``test_web_subdomain_hosting.py`` starts its own).
    """
    nest, cleanup = _start_dedicated_nest(
        request,
        nest_mode,
        tmp_path_factory,
        "web-hosting-nest",
        claim_domain=WEB_HOSTING_DOMAIN,
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def web_hosting_spa_url(static_dir, web_hosting_nest):
    """Function-scoped SPA proxy → ``web_hosting_nest``, the web twin of pointing
    a native driver straight at that nest's url. Mirrors
    ``dedicated_no_mail_spa_url``; unused until the web leg of the published-post
    surface lands, and present so that leg needs no conftest change."""
    url, server = _serve_spa_proxy(static_dir, web_hosting_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def web_hosting_app(request, app, web_hosting_nest):
    """``app``, logged in as ``web_hosting_nest``'s actor — the fixture the
    published-post management journey runs on, because only a handle-domain nest
    gives the copy affordances an origin that actually serves."""
    _login_app_as(
        app,
        request,
        web_hosting_nest,
        web_hosting_nest["user"],
        spa_url_fixture="web_hosting_spa_url",
    )
    return app


@pytest.fixture
def dedicated_no_mail_app(request, app, dedicated_no_mail_nest):
    """``app``, logged in as the dedicated ``dedicated_no_mail_nest`` actor —
    guaranteed to hold no MSEK regardless of what any other test file has
    done to the shared session actor.

    Web reaches the dedicated nest through ``dedicated_no_mail_spa_url`` (NOT the
    shared ``spa_url``, which proxies ``nest_instance``): without it the browser
    would talk to the shared nest, where this actor is unregistered, and every RPC
    would fail ``fauna.auth.not_registered``. Native drivers ignore it and dial
    ``dedicated_no_mail_nest['url']`` directly."""
    _login_app_as(app, request, dedicated_no_mail_nest, dedicated_no_mail_nest["user"],
                  spa_url_fixture="dedicated_no_mail_spa_url")
    return app


@pytest.fixture(scope="module")
def spki_pinned_nest(request, nest_mode, tmp_path_factory):
    """``self_signed_nest``'s NON-LOOPBACK twin — the only nest that can drive a
    client's SPKI-**pin** trust branch.

    ``self_signed_nest`` binds ``127.0.0.1``, so a client trusts its self-signed
    cert on the **loopback** short-circuit and no pin is ever consulted
    (``ShouldTrust`` = ``webPkiValid || isLoopback || spki == pin``). That is the
    right proof for the same-box install, but it leaves the *remote* ``test@<ip>``
    case — which rides the pin — completely uncovered.

    This nest is dialled on the box's own LAN IP (``lan_ipv4()``), so:

    * the authority is non-loopback (``is_loopback_authority`` parses IP literals
      and never resolves DNS), hence ``isLoopback`` is FALSE;
    * the floor cert is self-signed, hence ``webPkiValid`` is FALSE;
    * so trust can succeed by ONE mechanism only — ``spki == pin``. Any leg that
      lands here proves the pin branch, by elimination.

    The cert's SANs are ``localhost`` + ``127.0.0.1`` only, so this dial is also
    name-mismatched — deliberately. Neither verifier gates on SAN (the Rust
    ``CapturingVerifier`` is ``AcceptProvisional``; .NET hands the callback the
    final say), so the mismatch is policy input, not a hard failure. That mirrors
    production: an IP-dialled nest never has a matching SAN.

    Packets never leave the box, and Windows Firewall does not filter same-host
    traffic to the host's own IP — so this needs no elevation and no firewall rule.
    """
    from common.nest import lan_ipv4
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "spki-pinned-nest",
        serve_tls=True, dial_host=lan_ipv4(),
    )
    _assert_pin_branch_authority(nest["url"])
    _skip_if_lan_self_dial_is_broken(nest["url"])
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


def _skip_if_lan_self_dial_is_broken(url: str) -> None:
    """Preflight the box's ability to complete a TLS handshake against its own
    LAN IP before trusting any test result built on ``spki_pinned_nest``.

    Found 2026-08-28: on at least one macOS VM, a TCP connect
    to the box's own advertised LAN IP (`common.nest.lan_ipv4()`) succeeds —
    plain sockets, and a loopback-dialled TLS handshake on the SAME nest
    process both work fine — but the TLS handshake on the LAN-IP-dialled
    connection dies with `ssl.SSLEOFError` client-side / `Socket is not
    connected (os error 57)` server-side, between `accept()` and the
    acceptor's first read. Isolated with `RUST_LOG=debug`: unrelated to
    `fauna_conn_limit::arm_dead_peer_detection` (confirmed by disabling that
    call entirely — the failure persisted identically), so it is not a Fauna
    code path at all; it reproduces identically against a bare Rust
    `tokio::net::TcpListener` accept loop, which is the signature of a
    kqueue/mio non-blocking-accept quirk specific to this VM's hairpin-routed
    (self-addressed, actually carried over `lo0`) self-dial — a raw
    *blocking* Python socket doing the identical accept+read does NOT
    reproduce it. This is a box property, not an app property (`app_surface.
    skip_environment`'s own charter) — never silently hide it as a green nor
    let it masquerade as a per-app regression on every future baseline.
    """
    host, port = _split_authority(url)
    try:
        ctx = ssl._create_unverified_context()
        with socket.create_connection((host, port), timeout=10) as sock:
            with ctx.wrap_socket(sock, server_hostname=host):
                pass
    except OSError as e:
        from helpers.app_surface import skip_environment
        skip_environment(
            f"this box cannot complete a TLS handshake against its own LAN IP "
            f"({host}:{port}): {e!r} — a plain TCP connect and a loopback TLS "
            "handshake on the same nest both work, so this is a VM-networking/"
            "kqueue self-dial quirk (see this function's docstring), not an "
            "app or nest defect"
        )


def _split_authority(url: str) -> tuple:
    host, _, port = url.split("://", 1)[1].rpartition(":")
    return host, int(port)


def _assert_pin_branch_authority(url: str) -> None:
    """Guard the whole premise of ``spki_pinned_nest``: if the authority were
    loopback after all, every assertion downstream would still pass — on the
    loopback short-circuit — and the test would claim to prove the pin branch
    while proving nothing. Fail loudly instead of greening vacuously.
    """
    from common.nest import _is_loopback_host
    assert url.startswith("https://"), f"pin branch needs real TLS, got {url!r}"
    authority = url.split("://", 1)[1]
    host = authority.rsplit(":", 1)[0]
    assert not _is_loopback_host(host), (
        f"{url!r} is a LOOPBACK authority — a client would short-circuit to trust "
        "before consulting any pin, so this fixture would prove nothing. It must "
        "be dialled on a non-loopback host."
    )


@pytest.fixture
def spki_pinned_logged_in_app(request, app, spki_pinned_nest):
    """``app``, logged in against the non-loopback self-signed nest.

    Logging in mints the bearer over WS-RPC (shared-Rust ``mint_bearer`` →
    ``graduate_handshake``), which WRITES the pin for this authority into the
    process-global store in the statically-linked ``fauna_ffi`` DLL. The C# HTTP
    callback (``PinnedSpkiForHost``) READS that same store under the same
    ``authority_of`` key — so the login is what arms the pin the residual-HTTP legs
    then rely on. No extra wiring: it is one in-process ``OnceLock``.

    web declares the whole leg absent FIRST — ``_declare_no_web_cert_trust``,
    before the login attempt, because a declaration placed after the thing it
    declares absent never fires.
    """
    _declare_no_web_cert_trust(app)
    _login_app_as(app, request, spki_pinned_nest, spki_pinned_nest["user"])
    return app


# A valid 64-char-hex (32-byte) device id for the e2e logged-in session. Real
# device ids ARE hex — a hex-encoded 32-byte id (linux `hex::encode(device_id())`,
# the seed fixtures' `blake2b(..., digest_size=32).hexdigest()`). The shared
# `MediaMachine` upload gesture hex-decodes `device_id` to register + record the
# member, so a non-hex placeholder (the old "test-device-login") fails
# `invalid device_id hex` on any client that reads the session device id for an
# upload (windows). Distinct from every seeded actor's per-actor device, so the
# logged-in device is unregistered → it exercises the write self-heal.
_E2E_LOGIN_DEVICE_ID = "0123456789abcdef" * 4

#: The device id for the RECIPIENT seat of a two-seat share fixture, distinct from
#: the owner's `_E2E_LOGIN_DEVICE_ID` — same shape, different device, mirroring the
#: two-device pattern above (`"fedcba9876543210" * 4`).
#:
#: Two seats sharing one device id is not a cosmetic collision. The sync engine's
#: peer-vs-self-echo split is a per-(actor, device) question, so a member holding the
#: owner's device id classifies the owner's upload as its OWN echo: it applies
#: nothing, the anchor advances past the row anyway, and every later pull is
#: legitimately empty — the file never arrives and cannot be re-offered. That is
#: exactly what `test_writer_member_decrypts_owner_upload` had been failing on since
#: the S9 path-sealing flip; with
#: one id the test could never have exercised the cross-seat path it exists to pin.
_E2E_SHARE_RECIPIENT_DEVICE_ID = "89abcdef01234567" * 4
#: The third seat's id — a person the set was never shared with
#: (``folder_share_stranger_app``). Distinct from both for the same reason.
_E2E_SHARE_STRANGER_DEVICE_ID = "13579bdf02468ace" * 4


def _login_app_as(app, request, nest_instance, user, spa_url_fixture="spa_url",
                  *, verify_live_actor=False, device_id=None):
    """Log ``app`` in as ``user`` (a ``_make_user`` dict — the shared session
    ``test_user`` or a dedicated seeded actor). Uses ``set_state()`` for
    bridge-backed drivers with a test agent, UI login otherwise. For web the
    ``node_url`` must be a CORS-adding SPA proxy URL (the browser fetches relative
    to it), NOT ``nest_instance['url']``. The default ``spa_url_fixture`` proxies
    the shared session ``nest_instance``; a caller logging in against a *dedicated*
    nest MUST pass that nest's own proxy fixture (e.g. ``dedicated_no_mail_spa_url``)
    — otherwise the browser talks to the shared nest, where the dedicated actor is
    unregistered, and every RPC fails ``fauna.auth.not_registered`` (WS-RPC never
    connects). Native drivers point straight at ``nest_instance['url']`` and ignore
    ``spa_url_fixture``. Extracted from ``logged_in_app`` so seeded-actor fixtures
    reuse one login path (priority #2).

    An ``https://`` nest (docker mode, always) needs nothing special here: the
    SPA proxy's two hops both carry the run's floor-cert posture
    (``_serve_spa_proxy``), so a web login against the image's nest goes through
    exactly as against a plain standalone one. The web cert-trust declaration is
    NOT made here — it belongs to the two dedicated ``self_signed_nest``/
    ``spki_pinned_nest`` fixtures alone (``_declare_no_web_cert_trust``); made
    here, from 2026-09-01 to 2026-09-09, it declared the whole docker-mode web
    column absent for a property no ordinary journey asserts.

    ``verify_live_actor`` adds a barrier on the SWITCH ITSELF: poll the app's
    **live** identity (Settings → Status ``account-actor-id``, fed by a real
    authenticated session) until it is ``user``, then restore the feed landing.
    Off by default because the shared ``test_user`` re-logs in as the SAME actor
    on nearly every test in the suite, where the barrier can only cost time;
    **on for a fixture whose whole premise is a DEDICATED actor**, where a
    silently-declined switch leaves the app running as the previous test's actor
    and every read comes back correct-but-empty for it — the caller then reads
    an empty page and blames the feature. Note this
    cannot be checked with ``get_state("session")``: that block is the agent's
    own override of what the patch asked for (e2e-conventions.md convention 11
    § *A HALF-APPLIED command is a dropped command*)."""
    secret_hex = user["signing_key"].encode().hex()
    from drivers.http_bridge import HttpBridgeDriver
    if isinstance(app.driver, HttpBridgeDriver):
        # A nest this app's launch did not name must be named by a relaunch
        # BEFORE the session points there: escrow trust is captured at
        # launch (`_relaunch_trusting_nest` — a no-op for the launch nest).
        _relaunch_trusting_nest(app.driver, nest_instance)
        if app.driver.is_web():
            node_url = request.getfixturevalue(spa_url_fixture)
        else:
            node_url = nest_instance["url"]
        app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                # The actor's REAL handle on the nest, not a fabricated one. An
                # app's mail `From:` is `<handle>@<domain>` and the nest does
                # sender-handle verification (`smtp-server.md` § First-party
                # clients), so a session handle no nest row backs makes every
                # send an unownable claim the nest refuses. The hardcoded
                # `"e2e-user"` that used to sit here was exactly that.
                "handle": user.get("handle") or "e2e-user",
                "secret_hex": secret_hex,
                "actor_id": user["actor_id_hex"],
                # `device_id=` names another id for this one sign-in — the
                # device-roster tests pass one the app refuses to adopt, to
                # reach its own get-or-create (`helpers/enrollment.py`).
                "device_id": device_id or _E2E_LOGIN_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
    else:
        app.auth.login(
            node_url=nest_instance["url"],
            username=user["actor_id_hex"],
            password=secret_hex,
            secret_hex=secret_hex,
        )

    # The CONNECTION BARRIER — one per login, not one per action.
    #
    # Every app desensitizes an `OnlineOnly` affordance while its transport word
    # is offline, and `"connecting"` is offline. 276 of the 628 registered wire
    # kinds are `OnlineOnly`, so without this a test that drives an online-only
    # control right after login is racing the WS handshake and loses exactly
    # when the box is loaded — the load-dependent flake class conventions
    # point 14 forbids. It sits at the login seam rather than inside individual
    # actions for the same reason `calendar_backend` is a fixture: it is a
    # precondition of the *app*, not of one caller.
    #
    # Latency-independent (a deadline poll on the app's own published state,
    # never a settle-sleep) and a no-op on an app whose leg has not landed —
    # `helpers/connection.py` owns both rules.
    from helpers.connection import wait_until_online

    wait_until_online(app.driver)

    if verify_live_actor:
        from helpers.e2e_session import wait_live_actor_id

        expected = user["actor_id_hex"]
        seen = wait_live_actor_id(app, expected)
        assert expected.lower() in seen.lower(), (
            f"the login as the dedicated actor never reached the LIVE session: "
            f"account-actor-id={seen!r}, expected {expected!r}. The app is still "
            f"running as the PREVIOUS test's actor, so every caller-scoped read "
            f"below is answered correctly and EMPTY for that actor — which reads "
            f"as a broken feature rather than a dropped login "
            f"(e2e-conventions.md convention 11)."
        )
        # Restore the landing the session patch asked for; the barrier above
        # left the app on a Settings sub-page.
        app.driver.navigate_to("feed")


def _seed_cross_set_media(nest_instance, user, plan, *, device_id=None):
    """Seed ``plan`` (``{set_name: [paths]}``) into ``user``'s owned folders via
    the **real** ``fauna.sync.changes.record`` RPC — the production path the
    ``fauna-sync`` daemon itself uses, NOT a spawned daemon (heavy/flaky) and NOT a
    test backdoor. Registers a write-capable device, creates each set (mode=sync),
    and records a ``create`` change with a non-null manifest per path (which is all
    ``fauna.media.list`` reads — no real chunk/blob upload, mirroring the Rust
    ``conformance_media_list`` seed). Self-checks the data path via
    ``fauna.media.list`` so a seed failure fails HERE (loud), not as a downstream
    empty-UI flake. Returns ``plan``.

    ``device_id`` (hex) seeds through a device the caller registers anyway, so
    a fixture that records more files of its own spends one device slot, not
    two: the account's tier counts every registered device, and on a live box
    the `free` tier's cap is small enough that the app's own machine is then
    refused registration (the shared-box rule)."""
    import hashlib

    from common.auth import (
        _user_call,
        sync_changes_record,
        sync_register,
        user_create_folder,
    )

    port = nest_instance["port"]
    url = nest_instance["url"]
    secret_key = user["signing_key"].encode().hex()
    # Per-actor device id (registration is keyed on device_id, not actor, and the
    # session nest is shared) so each seeded actor registers its own write-capable
    # device rather than relying on a globally-shared one.
    if device_id is None:
        device_id = hashlib.blake2b(b"seed-device:" + secret_key.encode(),
                                    digest_size=32).hexdigest()

    sync_register(port, secret_key=secret_key, device_id=device_id,
                  capabilities="read,write", base_url=url)

    total = 0
    for set_name, paths in plan.items():
        user_create_folder(port, set_name, secret_key=secret_key, base_url=url)
        for p in paths:
            manifest = hashlib.blake2b(f"manifest:{p}".encode(),
                                       digest_size=32).hexdigest()
            sync_changes_record(port, secret_key=secret_key, folder=set_name,
                                device_id=device_id, path=p, manifest_hash=manifest,
                                size_bytes=1024, change_type="create", base_url=url)
            total += 1

    # Data-path self-check before any UI loads: media.list must surface the seed.
    reply = _user_call(port, secret_key, "fauna.media.list", {"limit": 1000, "cursor_version": 2}, url)
    items = reply.get("items", [])
    assert len(items) == total, (
        f"media seed self-check: fauna.media.list returned {len(items)} items, "
        f"expected {total} across {list(plan)}; reply={reply!r}"
    )
    return plan


@pytest.fixture
def seeded_media_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED fresh actor whose ≥2 owned folders are
    pre-seeded with media via the real ``fauna.sync.changes.record`` RPC (no sync
    daemon, no test backdoor — the production path the daemon itself uses).

    Dedicated (NOT the shared session ``test_user``) on purpose: ``fauna.media.list``
    is scoped to the connection actor's readable sets, so (a) the seed can't pollute
    the empty-state ``test_media_explorer_chrome`` (which logs in as ``test_user``),
    and (b) this actor sees *exactly* the seeded sets → clean ``==`` count asserts.

    Returns ``(app, plan)`` where ``plan`` is ``{set_name: [paths]}``."""
    app, plan, _user = _seeded_media_login(request, app, nest_instance)
    return app, plan


@pytest.fixture
def seeded_media_app_and_user(request, app, nest_instance):
    """``seeded_media_app`` plus the dedicated actor it logged in as —
    ``(app, plan, user)`` — for a journey that must launch ANOTHER seat of that
    same account (``alice_builder_seats.launch(user=…)``)."""
    return _seeded_media_login(request, app, nest_instance)


def _seeded_media_login(request, app, nest_instance):
    """The body of ``seeded_media_app``: mint the dedicated actor, seed its two
    folders, log ``app`` in as it. Returns ``(app, plan, user)``."""
    user = _make_user(nest_instance)
    plan = _seed_cross_set_media(nest_instance, user, {
        "media-seed-photos": ["photos/sunset.jpg", "photos/forest.png"],
        "media-seed-clips": ["clips/intro.mp4"],
    })
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return app, plan, user


#: The ``sortable_media_app`` seed: ``(path, size_bytes)`` in RECORD order, oldest
#: first. Chosen so each sort key gives a DIFFERENT order — name a·b·c, size
#: c·a·b, date b·c·a — and none of them is the ``(folder, path)`` tiebreak order
#: except name itself, so a key that silently fell back to the tiebreak (equal
#: dates included) reads as the wrong order rather than passing by coincidence.
SORTABLE_MEDIA = (
    ("sortable/banana.bin", 3000),
    ("sortable/cherry.bin", 1000),
    ("sortable/apple.bin", 2000),
)


@pytest.fixture
def sortable_media_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED fresh actor owning ONE ``sync``-mode set of
    three files that differ in name, size AND date — the one shape an ordering
    assertion over ``media-sort-select`` can stand on.

    ``seeded_media_app`` cannot: it records every file at 1024 bytes within the
    same second, so a size or date sort there leaves the shared ``(folder, path)``
    tiebreak (``fauna-client-media::sort_items``) to decide the order and proves
    nothing about the key.

    **Dates are the nest's own record time** (``updated_at``, unix seconds — the
    wire carries no client-chosen mtime), so distinct dates need records landing in
    distinct seconds. Each record waits for the clock to pass the previous one's
    recorded second, read back from ``fauna.media.list`` itself — a causal anchor on
    the stamp the sort will use, not a fixed delay — and the self-check at the end
    refuses a seed whose dates did not come out strictly increasing.

    Same production seam as ``_seed_cross_set_media`` (``fauna.sync.changes.record``,
    owner-root sealed paths), so every row renders on every app.

    Returns ``(app, set_name, rows)`` where ``rows`` is :data:`SORTABLE_MEDIA`'s
    ``(display name, size_bytes)`` pairs in record order."""
    import hashlib

    from common.auth import (
        _user_call,
        sync_changes_record,
        sync_register,
        user_create_folder,
    )

    port = nest_instance["port"]
    url = nest_instance["url"]
    user = _make_user(nest_instance)
    secret_key = user["signing_key"].encode().hex()
    device_id = hashlib.blake2b(b"sortable-media-device:" + secret_key.encode(),
                                digest_size=32).hexdigest()
    set_name = f"sortable-{secret_key[:8]}"
    sync_register(port, secret_key=secret_key, device_id=device_id,
                  capabilities="read,write", base_url=url)
    user_create_folder(port, set_name, secret_key=secret_key, base_url=url)

    def _stamps() -> dict:
        # Keyed by SIZE, which the seed makes distinct: the reply carries no
        # plaintext path (the path rests sealed — `sync_changes_record`'s S9 note),
        # so the size is the one field that says which record is which.
        reply = _user_call(port, secret_key, "fauna.media.list", {"limit": 1000, "cursor_version": 2}, url)
        return {it["size_bytes"]: it["updated_at"] for it in reply.get("items", [])}

    previous_stamp = None
    for path, size in SORTABLE_MEDIA:
        if previous_stamp is not None:
            # Land this record in a LATER second than the previous one's stamp.
            deadline = time.monotonic() + 10.0
            while int(time.time()) <= previous_stamp and time.monotonic() < deadline:
                time.sleep(0.05)
        manifest = hashlib.blake2b(f"manifest:{path}".encode(), digest_size=32).hexdigest()
        sync_changes_record(port, secret_key=secret_key, folder=set_name,
                            device_id=device_id, path=path, manifest_hash=manifest,
                            size_bytes=size, change_type="create", base_url=url)
        previous_stamp = _stamps().get(size)
        assert previous_stamp is not None, (
            f"sortable seed: {path!r} ({size} bytes) was recorded but fauna.media.list "
            f"lists no item of that size: {_stamps()!r}"
        )

    stamps = _stamps()
    ordered = [stamps.get(size) for _, size in SORTABLE_MEDIA]
    assert all(a is not None and b is not None and a < b
               for a, b in zip(ordered, ordered[1:])), (
        f"sortable seed self-check: the record dates must be strictly increasing in "
        f"record order, or a date sort cannot be told from the path tiebreak; "
        f"got {dict(zip([p for p, _ in SORTABLE_MEDIA], ordered))!r}"
    )
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    rows = [(path.rsplit("/", 1)[-1], size) for path, size in SORTABLE_MEDIA]
    return app, set_name, rows


@pytest.fixture
def live_media_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED fresh actor owning ONE **empty**
    ``sync``-mode set, plus a ``record_file(path)`` closure that lands a new file
    in that set over the real ``fauna.sync.changes.record`` RPC.

    The deliberate inverse of ``seeded_media_app``: that one seeds **before** the
    UI ever loads, so it can only ever prove the *first* read. This one keeps the
    write for **after** the page is mounted, which is the only way to observe the
    remote-change nudge (``docs/goal/behavior/file-sync.md`` § Remote-change
    nudge) reaching the Media page — a file arriving from the local sync engine,
    a second device, or a collaborator while the user is looking at it.

    ``record_file`` is the production path the ``fauna-sync`` daemon itself uses,
    not a test backdoor, and it is a *third-party* write by construction (its own
    registered device), so it is fixture/external-actor setup rather than a
    mutation the user performs — e2e convention 8's stated carve-out.

    Returns ``(app, set_name, record_file)``."""
    import hashlib

    from common.auth import (
        sync_changes_record,
        sync_register,
        user_create_folder,
    )

    port = nest_instance["port"]
    url = nest_instance["url"]
    user = _make_user(nest_instance)
    secret_key = user["signing_key"].encode().hex()
    device_id = hashlib.blake2b(b"live-media-device:" + secret_key.encode(),
                                digest_size=32).hexdigest()
    set_name = f"live-media-{secret_key[:8]}"

    sync_register(port, secret_key=secret_key, device_id=device_id,
                  capabilities="read,write", base_url=url)
    user_create_folder(port, set_name, secret_key=secret_key, base_url=url)

    def record_file(path):
        """Land one file in the set, as another device would. Returns ``path``."""
        manifest = hashlib.blake2b(f"manifest:{path}".encode(),
                                   digest_size=32).hexdigest()
        sync_changes_record(port, secret_key=secret_key, folder=set_name,
                            device_id=device_id, path=path, manifest_hash=manifest,
                            size_bytes=1024, change_type="create", base_url=url)
        return path

    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return app, set_name, record_file


@pytest.fixture
def empty_media_app(request, app, nest_instance):
    """``app`` logged in as a DEDICATED fresh actor with **no folders and no
    media** — the first-use state a real user meets right after onboarding.

    The deliberate inverse of ``seeded_media_app``: nothing is pre-seeded, so a
    test can walk the *first* folder into existence through the UI and then
    use it. ``testing.md`` convention 17 names pre-seeding past the empty /
    first-use state as the anti-pattern that hid the empty-set upload defect for
    as long as it did — every media upload test reached its set through a fixture
    that had already created one.

    Dedicated (NOT the shared session ``test_user``) because such a test
    **creates a folder**, and the shared user's emptiness is load-bearing for
    ``test_media_upload_no_set_error`` — mutating it would make that test's
    verdict depend on execution order."""
    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return app


def _wait_for_engine(driver, predicate, timeout=20.0):
    """Poll ``data.sync`` until ``predicate(sync_obj)``; return the last obj seen."""
    deadline = time.monotonic() + timeout
    sync = {}
    while time.monotonic() < deadline:
        state = driver.get_state() or {}
        sync = (state.get("data") or {}).get("sync") or {}
        if predicate(sync):
            return sync
        time.sleep(0.2)
    return sync


def _bind_live_location(app, watch_dir, folder, folder_id):
    """Bind ``watch_dir`` to the set ``folder`` (by its ref ``folder_id``) on the
    logged-in ``app`` and wait until the app's own engine serves it.

    The app's bound folder is the harness's SIGNED writer where a test has an
    app: its engine signs every change record it writes
    (``mls-group-key-material.md`` § M2 → *Writer-signed change records*), where
    the nest refuses the headless ``fauna-sync`` daemon's unsigned records. Live, without a sign-out, and by the set's ref,
    as the Folders UI's bind does — fixture setup arranging the world (e2e rule 8
    carve-out (b)); a test whose subject is the bind itself drives the UI.
    Returns the last ``data.sync`` read.
    """
    app.driver.call_command(
        "sync_add_location",
        {"path": str(watch_dir), "folder": folder, "folder_id": folder_id},
    )
    # 60 s, not the 20 s default: on windows the engine starts only after the
    # app's convergence loop pushes the capability to the out-of-process agent,
    # and that loop ticks on a ~30 s cadence — the bind itself lands first
    # (folders shows it) while `running` flips on the next provision tick.
    sync = _wait_for_engine(
        app.driver,
        lambda s: s.get("running") is True
        and folder in {f.get("folder") for f in s.get("locations", [])},
        timeout=60.0,
    )
    assert sync.get("running") is True, (
        f"the sync engine did not start after binding {watch_dir} -> {folder!r}, so "
        f"nothing will write the files this setup arranges. last data.sync: {sync}"
    )
    return sync


@pytest.fixture
def bound_location_media_app(request, app, nest_instance, tmp_path):
    """``app`` logged in as a DEDICATED fresh actor with a REAL local folder bound to
    a REAL ``sync``-mode folder, and the client's in-process ``SyncEngine`` running
    live against it.

    **Why this exists next to ``seeded_media_app``** — the two are opposite trades and
    the difference is the whole point. ``seeded_media_app`` fabricates manifests over
    ``fauna.sync.changes.record`` and **never maps a folder**, so no engine runs and
    nothing it backs can observe the **disk**; it is fast and right for explorer/
    projection assertions. This fixture pays a real chunk-upload round-trip so the
    file genuinely lands **on disk** and in the engine's ``SyncDb`` — the only shape
    that can witness delete *propagation* (``docs/goal/behavior/file-sync.md``
    § Files Appear Automatically).

    The engine's pull cadence is the harness's 30 s ``FAUNA_E2E_RESCAN_MS`` default
    (``drivers/tui.py`` / ``drivers/linux.py``): since phase 5 of the folders
    re-model no seat reads a cadence off the nest row — the production value is
    the 300 s constant, which would put the delete-propagation assertion out of
    reach of any sane timeout, so the seam is load-bearing here (the remote-change
    nudge usually lands the tombstone first; the tick is the backstop the budget
    is sized for). This fixture used to create the row with ``rescan_interval_secs=2``
    for the same purpose; that write went dead the day the de-knob landed.

    linux + windows (2026-07-22). On windows a REAL isolated ``fauna-sync-agent.exe``
    is spawned and its pipe confirmed serving BEFORE login — the
    ``sync_live_apply_app`` treatment (``test_sync_live_apply.py`` documents WHY the
    spawn must precede ``_login_app_as``: login starts the app's hydration loop, whose
    first tick would otherwise ``SpawnSyncAgentDetached`` onto the box's real per-SID
    pipe). Consumers therefore need the ``isolated_sync_agent`` + ``real_sync_agent``
    markers on windows.

    **The upload is a PRECONDITION, so it is arranged here, not asserted in the test**
    (e2e rule 8 carve-out (b) — fixture setup arranges the world; the behavior under
    test still rides the UI). Waiting for it here is also what keeps the consuming test
    honest about the page's refresh model: the Linux media page refreshes off WS-RPC
    when it **becomes visible** (`views/media/mod.rs` `connect_map`) and does not poll,
    so a test that navigates *before* the engine's upload lands fires its one refresh
    against an empty list and then waits forever on a page that will never re-poll —
    reading exactly like "the upload never landed" when the engine's own log says
    ``file uploaded``. Yield only once the upload is on the nest.

    Returns ``(app, tracked_file, folder)`` — ``tracked_file`` is on disk inside the
    bound folder AND uploaded/recorded by the live engine, so it is real to both the
    disk and ``fauna.media.list``.
    """
    from common.auth import _user_call, user_create_folder, user_folder_ref

    folder = "media-delete-disk-set"
    watch_dir = tmp_path / "bound-folder"
    watch_dir.mkdir()

    user = _make_user(nest_instance)
    secret_key = user["signing_key"].encode().hex()
    # The set must exist on the nest BEFORE the engine starts: engine startup reads
    # the folder roster once, and that single read drives both the reconcile cadence
    # and the content-key binding (`engine_lifecycle.rs` — build one engine).
    user_create_folder(
        nest_instance["port"],
        folder,
        secret_key=secret_key,
        base_url=nest_instance["url"],
    )
    folder_id = user_folder_ref(
        nest_instance["port"],
        folder,
        secret_key=secret_key,
        base_url=nest_instance["url"],
    )

    def _arrange():
        _login_app_as(app, request, nest_instance, user)
        _bind_live_location(app, watch_dir, folder, folder_id)

        # A real file for the real engine to watch, chunk, seal, upload and record.
        tracked_file = watch_dir / "doomed.txt"
        tracked_file.write_bytes(b"bytes the user expects to be gone after a delete\n")

        deadline = time.monotonic() + 90
        items = []
        while time.monotonic() < deadline:
            items = _user_call(
                nest_instance["port"], secret_key, "fauna.media.list", {"limit": 100, "cursor_version": 2},
                nest_instance["url"],
            ).get("items", [])
            if items:
                break
            time.sleep(0.5)
        assert len(items) == 1, (
            f"precondition: the live engine should have uploaded {tracked_file.name!r} into "
            f"{folder!r} and recorded it, but fauna.media.list returned {len(items)} items "
            f"({items!r}). The engine's upload/error lines do NOT reach the pytest log — read "
            f"the app's OWN log for `file uploaded` (the linux driver's per-launch "
            f"`<tmp>/app.err`; RUST_LOG propagates into the app via os.environ; on windows "
            f"the agent's own log is `<its --data-dir>/logs/fauna.log.<date>`, folded into "
            f"the windows driver's `app_stderr_text()`)."
        )
        return app, tracked_file, folder

    if app.driver.is_windows():
        from helpers.windows_sync_agent import serving_agent

        exe = request.getfixturevalue("sync_agent_binary")
        pipe_leaf = request.getfixturevalue("isolated_sync_agent_pipe_name")
        pipe_path = r"\\.\pipe" + "\\" + pipe_leaf
        # The launch's own agent, on the launch's own `--data-dir` — adopted when
        # the app already spawned it, started there otherwise; never a second
        # agent on a dir of our own, which exits as a duplicate while the pipe
        # goes on answering (`helpers/windows_sync_agent.serving_agent`).
        agent_data_dir = app.driver.sync_agent_state_base
        assert agent_data_dir, (
            "this launch pinned no agent `--data-dir` (`FAUNA_E2E_SYNC_AGENT_DATA_DIR`); "
            "the test must carry `isolated_sync_agent` (convention 10)"
        )
        with serving_agent(exe, pipe_path, agent_data_dir, tmp_path / "agent-spawn.log"):
            yield _arrange()
        return

    yield _arrange()


@pytest.fixture
def fresh_app(_driver_cache, spa_url):
    """An ActionLayer with NO identity — for onboarding/import flows.

    Reuses the session-scoped web driver. Resets state and navigates
    to the status page with empty session.
    """
    if "web" not in get_available_apps():
        pytest.skip("fresh_app requires the web client")
    driver = _driver_cache("web")
    driver.reset()
    driver.set_state({
        "session": {"node_url": spa_url},
        "nav": {"stack": [{"view": "status"}]},
    })
    import time
    time.sleep(2)
    layer = ActionLayer(driver)
    yield layer
    try:
        driver.screenshot(f"post-test-fresh-{id(driver)}")
    except Exception:
        pass


def _resolve_linux_binary():
    """Return the path to the fauna-linux debug binary, or None if not built."""
    # Check CARGO_TARGET_DIR first (e.g. $CARGO_TARGET_DIR)
    cargo_target = os.environ.get("CARGO_TARGET_DIR")
    if cargo_target:
        p = Path(cargo_target) / "debug" / "fauna-desktop"
        if p.exists():
            return p
    # Fall back to repo-relative path
    path = _repo_root / APP_PATHS["linux"]
    if path.exists():
        return path
    return None


def _resolve_cli_binary():
    """Return the path to the fauna-tui debug binary, or None if not built.

    Windows cargo emits ``fauna-tui.exe`` — ``APP_PATHS["tui"]`` is the POSIX
    basename, so resolve the platform-correct name here (else the built binary
    is never found on Windows and every ``--client tui`` case skips on it)."""
    exe = "fauna-tui.exe" if _IS_WINDOWS else "fauna-tui"
    cargo_target = os.environ.get("CARGO_TARGET_DIR")
    if cargo_target:
        p = Path(cargo_target) / "debug" / exe
        if p.exists():
            return p
    path = _repo_root / "target" / "debug" / exe
    if path.exists():
        return path
    return None


@pytest.fixture(scope="session")
def linux_app_path():
    """Path to the built ``fauna-desktop`` debug binary, for tests that launch a
    Linux driver DIRECTLY (not via the cached ``app`` fixture) — e.g. a
    degraded-nest launch that must seed credentials before boot.

    BUILDS rather than merely skipping — the ``macos_app_path`` shape, adopted
    2026-08-27 together with ``tui_app_path``, where the cost of not doing so
    was measured (see its docstring): a direct-launch suite never touches the
    ``app`` fixture, so nothing else in its run triggers ``_ensure_app_built``,
    and the binary it drives is whatever was last built. ``just linux-debug``
    is build-if-stale-gated, so this is ~free on a warm tree, and
    ``_prebuild_binaries`` runs it at collection time (outside every per-test
    budget) whenever an item requests this fixture. Skips only when the build
    still leaves no binary."""
    from helpers.app_surface import skip_unbuilt

    _ensure_app_built("linux")
    binary = _resolve_linux_binary()
    if binary is None:
        skip_unbuilt(
            "linux",
            surface="the fauna-desktop binary",
            detail="run 'just linux-debug' first",
        )
    return str(binary)


@pytest.fixture(scope="session")
def macos_app_path():
    """Path to the built bare ``FaunaMacOS`` executable, for tests that launch a macOS
    driver DIRECTLY (not via the cached ``app`` fixture) — e.g. the real-binary
    version-skew grid, which launches an *arbitrary* build (HEAD's or the pinned
    previous release's) and so cannot go through the cached-app path at all.

    BUILDS rather than merely skipping (since 2026-08-27 every ``*_app_path`` fixture
    but android's does): the direct-launch suites never touch the ``app`` fixture, so
    nothing else in their run would trigger ``_ensure_app_built``, and a silent skip
    would read as "the apple leg is covered" when it never ran. ``just mac-debug`` is build-if-stale-gated, so this is ~free when
    the tree is already warm."""
    from helpers.app_surface import skip_unbuilt

    _ensure_app_built("macos")
    path = _repo_root / APP_PATHS["macos"]
    if not path.exists():
        skip_unbuilt(
            "macos",
            surface="the macOS app binary",
            detail="run 'just mac-debug' first",
        )
    return str(path)


@pytest.fixture(scope="session")
def windows_app_path():
    """Path to the built ``FaunaApp.exe``, for tests that launch a Windows driver
    DIRECTLY (not via the cached ``app`` fixture) — e.g. the real-binary version-skew
    grid, which launches an ARBITRARY build (HEAD's or the pinned previous release's)
    and so cannot go through the cached-app path at all.

    BUILDS rather than merely skipping — mirrors ``macos_app_path`` for the same
    reason (the direct-launch suites never touch the
    ``app`` fixture, so nothing else in their run would trigger ``_ensure_app_built``,
    and a silent skip would read as "the windows leg is covered" when it never ran).
    ``just windows-debug`` is build-if-stale-gated, so this is ~free when the tree is
    already warm."""
    from helpers.app_surface import skip_unbuilt

    _ensure_app_built("windows")
    path = _resolve_windows_app()
    if path is None:
        skip_unbuilt(
            "windows",
            surface="the Windows app binary",
            detail="run 'just windows-debug' first",
        )
    return str(path)


@pytest.fixture(scope="session")
def ios_setup():
    """``{"udid": ..., "app_path": ...}`` for tests that launch an iOS driver DIRECTLY
    (not via the cached ``app`` fixture) — the iOS twin of ``macos_app_path`` (e.g. the
    account-switcher suite, which owns its driver so a test-scoped credential seed
    reaches the app before launch).

    Builds rather than merely skipping, same rationale as ``macos_app_path``: a
    direct-launch suite never touches the ``app`` fixture, so nothing else in its run
    would trigger the iOS build — a silent skip would read as "the iOS leg is covered"
    when it never ran. Delegates to the session-memoized ``_get_ios_setup()`` (the same
    simulator-boot + ``xcodebuild`` path ``_build_app_config`` uses), so this is
    ~free once that cache is warm.
    """
    from helpers.app_surface import skip_unbuilt

    _ensure_app_built("ios")
    setup = _get_ios_setup()
    if setup is None:
        skip_unbuilt(
            "ios",
            surface="the iOS simulator/app",
            detail="run 'just apple-ffi-test' first",
        )
    return setup


@pytest.fixture(scope="session")
def tui_app_path():
    """Path to the built ``fauna-tui`` debug binary, for tests that launch a tui
    driver DIRECTLY (not via the cached ``app`` fixture) — the launch-routing
    cases, which must seed credentials before boot.

    BUILDS rather than merely skipping (the ``macos_app_path`` shape), since
    2026-08-27 — measured the hard way on ``test_account_instance_lock_tui.py``'s
    bound-seat succession case: a direct-launch suite never touches the ``app``
    fixture, so nothing else in its run triggers ``_ensure_app_built``, and the
    binary it drives is whatever was last built. A shared-Rust fix landed in
    the tree, the journey re-ran, and its "green" run drove a 55-minute-old
    binary that failed on the pre-fix shape — indistinguishable, from the
    verdict alone, from the fix not working. A silent skip reads as covered; a
    stale binary reads as a VERDICT. ``just tui-debug`` is build-if-stale-gated,
    so this is ~free on a warm tree, and ``_prebuild_binaries`` runs it at
    collection time (outside every per-test budget) whenever an item requests
    this fixture. Skips only when the build still leaves no binary.

    Named ``tui_``, not ``cli_``: the modules that reach it do so by *interpolating
    the client id* (``request.getfixturevalue(f"{client}_app_path")``), so the
    `cli` → `tui` rename silently broke every tui case at SETUP —
    they collected fine and then errored `fixture 'tui_app_path' not found`. The
    rename swept `_SUPPORTED_APPS` and `cred_store.py` but missed this one."""
    from helpers.app_surface import skip_unbuilt

    _ensure_app_built("tui")
    binary = _resolve_cli_binary()
    if binary is None:
        skip_unbuilt(
            "tui",
            surface="the fauna-tui binary",
            detail="run 'cargo build -p fauna-tui' first",
        )
    return str(binary)


@pytest.fixture(scope="session")
def android_app_path():
    """Path to the built Android debug APK, for tests that launch an Android
    driver DIRECTLY (not via the cached ``app`` fixture) — e.g. the custody
    alarm suite (`test_atproto_custody_alarm.py`), which passes an explicit
    `environment` dict at launch. Skips when the APK isn't built — the one
    ``*_app_path`` fixture that does NOT trigger a build (the other four do,
    since 2026-08-27): `just android-debug`
    (FFI + Gradle `assembleDebug`/`assembleDebugAndroidTest`) is heavier than a
    path fixture should invoke on its own, and (per `_android_available()`)
    android e2e additionally needs a connected `adb` device, which no build
    step can provide."""
    from helpers.app_surface import skip_unbuilt

    path = _repo_root / APP_PATHS["android"]
    if not path.exists():
        skip_unbuilt(
            "android",
            surface="the Android debug APK",
            detail="run 'just android-debug' first",
        )
    return str(path)


def pytest_addoption(parser):
    parser.addoption("--app", action="store", default=None,
                     help="Comma-separated list of apps to test (e.g. --app ios,web). "
                          "When set, deselects tests that don't exercise the chosen app(s). "
                          "See tests/e2e-unified/README.md for details.")
    # Deprecated alias kept by the 2026-07-29 clients→apps rename so the
    # fleet's documented `--client <name>` commands keep working.
    parser.addoption("--client", action="store", default=None,
                     help="Deprecated alias for --app.")
    parser.addoption("--include-independent", action="store_true", default=False,
                     help="With --client, also run client-independent tests "
                          "(api contract, driver unit tests, scenarios, etc.)")
    parser.addoption("--tier", action="store", default=None,
                     help="Mocking-depth tier filter: 1 (in-process unit), 2 (driver + mocked backend), "
                          "3 (full stack, every binary real — locally-built binaries), "
                          "4 (full Docker deployment image + compose sidecars — slowest). "
                          "Comma-separated for multiple. "
                          "See tests/e2e-unified/README.md § Test tiers. Distinct from --client and from the "
                          "feature-breadth `tier0/1/2` markers (smoke/core/extended journeys).")
    parser.addoption("--macos-artifact", action="store_true", default=False,
                     help="Drive the SHIPPED macOS artifact — the real `Fauna.app` bundle "
                          "`just mac-app debug` assembles (and `mac-dmg` / the installer "
                          ".pkg ship) — instead of the bare swift-build binary the default "
                          "path launches. Run-level and single-valued, like `--nest docker`: "
                          "the mode swaps what `--app macos` launches, so any macOS journey "
                          "can be replayed against the artifact. Also the collection gate for "
                          "tests/artifact/ — without this flag that directory is never "
                          "imported, so no default sweep or tier filter can reach it. Each "
                          "launch stages a private copy under a per-instance "
                          "CFBundleIdentifier (the fixed id is what wedged WindowServer — "
                          "apple-e2e-automation.md § Artifact launch mode). Opt-in, never "
                          "the inner loop: see `just e2e-macos-artifact-test`. Implies "
                          "--client-artifact.")
    parser.addoption("--client-artifact", action="store_true", default=False,
                     help="Collection gate for tests/artifact/ — the shipped-CLIENT-artifact "
                          "category (testing.md § The four-tier taxonomy point 4), whose subject "
                          "is what an install/packaging channel produced rather than what the "
                          "build tree holds. Without it (or --macos-artifact, which implies it) "
                          "that directory is never imported, so no default sweep, tier filter or "
                          "`just` recipe can reach it. The directory's own conftest then prunes "
                          "each module whose OS is not this box's, so this flag collects the "
                          "apple modules on a Mac and the linux one on a Linux box. Unlike "
                          "--macos-artifact it swaps no launch mode by itself: the linux module "
                          "selects its subject through the `installed_product` marker "
                          "(_installed_linux_app_override). Opt-in, never the inner loop: see "
                          "`just e2e-linux-artifact-test`.")
    parser.addoption("--real-session", action="store_true", default=False,
                     help="Opt in to tests/real_session/ — the real-ambient-session category: "
                          "tests that deliberately run against and MUTATE the box's real desktop "
                          "session (real `systemd --user` manager, real session D-Bus, real "
                          "~/.config) instead of the harness's per-launch isolation (testing.md "
                          "§ point 10, which still governs every other suite). Without this flag "
                          "the directory is never even collected, so no default sweep can reach "
                          "it. Session-exclusive: the autouse lock in "
                          "tests/real_session/conftest.py serializes the box's desktop session "
                          "machine-wide. See testing.md § Real-ambient-session tests.")
    parser.addoption("--walk-sweep", action="store_true", default=False,
                     help="Opt in to tests/walk/ — convention 17's LAYER (c) UI walk sweep: a "
                          "systematic walk of every canonical page's focus ring over the real "
                          "binary, through the generic walk commands (`focus_move`/`switch_pane`), "
                          "asserting the layer-(b) invariant catalogue after every step. "
                          "Convention 17 scopes layer (c) to SCHEDULED SWEEPS ONLY, never the "
                          "inner loop — breadth belongs at tier_1 (layer (a)) where a step costs "
                          "microseconds instead of a round trip — so without this flag the "
                          "directory is never even collected. See e2e-conventions.md § convention "
                          "17 and helpers/ui_walk.py.")
    parser.addoption("--reclaim-cycle", action="store_true", default=False,
                     help="Put the session's shared `nest_instance` through ONE real "
                          "factory-reset -> re-claim (same identity) cycle BEFORE any test or "
                          "downstream fixture runs, so every suite that uses the shared nest "
                          "exercises post-reclaim provisioning (the 'works-on-first-claim, "
                          "breaks-after-re-claim' bug class). Pair with `-m reclaim_cycle` to run "
                          "the curated reclaim-sensitive gate, or run any suite under it to test "
                          "that suite post-reclaim. Pair with `just e2e-reclaim-cycle-test`.")
    parser.addoption("--strict-app", action="store_true", default=False,
                     help="Turn every UNBUILT-SURFACE skip into a failure, so a coverage claim "
                          "for the app under test is falsifiable (testing.md § Cross-app e2e "
                          "conventions, convention 7). A test that collects and then skips "
                          "reports `s` inside a summary line that reads like success — this flag "
                          "is what tells `tui runs this` from `tui skips this quietly`. Only "
                          "`helpers/app_surface.py::skip_unbuilt` is affected: a "
                          "`declared_absence` (a goal-doc-declared platform absence) and a "
                          "`skip_environment` (no network, missing live credential) still skip, "
                          "because no app work would make them run. Every run — flag or not — "
                          "prints the unbuilt-skip tally at the end; that number is a ratchet.")
    parser.addoption("--permissive-actuation", action="store_true", default=False,
                     help="apple, tui, linux: opt OUT of the actuation gate for this run. "
                          "By default the in-process automation server REFUSES to actuate a "
                          "control the real UI has disabled "
                          "(`/element/{click,double_click,type,clear,select}` consult the "
                          "element's live enabled state and answer HTTP 409 instead of "
                          "driving it) — the default on apple since 2026-08-05 (after both "
                          "targets swept clean) and on tui, which has refused a disabled "
                          "click since its automation surface existed and gated all five "
                          "routes 2026-08-21, and on linux since its 2026-09-10 sweep. With "
                          "this flag the server drives the control anyway and logs a "
                          "`DISABLED-ACTUATION` marker per call, which is how you ENUMERATE "
                          "violations: a refusal reds its test and a red test stops, so a "
                          "default run reports at most the first offender per test, while a "
                          "permissive sweep turns nothing red and finds them all. Pair it "
                          "with --actuation-log for a whole-suite harvest. Sets "
                          "FAUNA_E2E_PERMISSIVE_ACTUATION for the app launch; no-op for the "
                          "apps with no in-process gate.")
    parser.addoption("--actuation-log", action="store", default=None, metavar="PATH",
                     help="apple, tui, linux: accumulate every DISABLED-ACTUATION marker into PATH, "
                          "one run-scoped file that OUTLIVES the per-module cold relaunch "
                          "(testing.md convention 10) — which the app's own stderr does not, "
                          "since `app.err` dies with its launch's temp dir. This is what makes "
                          "one PERMISSIVE sweep (--permissive-actuation) enumerate every offender "
                          "in one pass: strict mode reds stop their test at its FIRST offender "
                          "and hide the rest, and a permissive pass turns nothing red, so it "
                          "doubles as the pre-existing-red baseline. Each test is delimited in "
                          "the file by a `=== TEST <nodeid>` line, so every marker is attributed "
                          "to the test that caused it. The marker text is shared verbatim "
                          "across the three hosts (the Rust half is `fauna_e2e_agent`, which "
                          "tui and linux both host; the Swift half is "
                          "`InProcessAutomationServer`), so one grep spans a cross-app sweep. "
                          "Sets FAUNA_E2E_ACTUATION_LOG for the app launch; no-op for the "
                          "apps with no in-process gate.")
    parser.addoption("--nest", action="store", default=None,
                     help="Which nest fills the real-nest slot this run: "
                          "standalone (default — the locally-built fauna-nest binary; "
                          "seconds to start, fresh private state, the red-green inner loop), "
                          "docker[:IMAGE] (the real image + docker/ sidecars under s6 — the "
                          "deployment artifact, provable before a production release), or "
                          "live[:URL] (an already-deployed real box, default example.com — "
                          "shared and STATEFUL, the only mode that catches works-on-fresh-only "
                          "bugs). Run-level and single-valued: multi-mode coverage composes as "
                          "separate pytest invocations via `just` recipes, never as an "
                          "inner-loop multiplier. The mode is stamped into every nest-dependent "
                          "test's id (test_x[tui-docker]). E2E_NEST is the env equivalent. "
                          "See testing.md § Default app and nest mode.")
    parser.addoption("--live-box", action="store", default=None,
                     metavar="shared|disposable",
                     help="What the live box this run hits is: shared (default — a human "
                          "may be signed into it, so exclusion class (3) deselects every "
                          "global-admin-mutating test under the shared-box rule) or "
                          "disposable (nobody is signed in and the run may wipe it, so "
                          "class (3) is off for the run — the CD gate's case; `just "
                          "e2e-cd-gate` passes it). Accepted only beside `--nest live` and "
                          "only for a staging box the tree names (dev.example.com, "
                          "test.example.com); refused for example.com or any other box. "
                          "Run-level, and deliberately WITHOUT an env equivalent: a "
                          "declaration that makes a run destructive sits in the invocation "
                          "someone reads, never in an ambient export. See testing.md § The "
                          "shared-box rule → The disposable-box declaration.")
    parser.addoption("--device-serial", action="store", default=None,
                     help="Which android device this run drives, as `adb devices` prints "
                          "its serial (e.g. --device-serial emulator-5554). Run-level and "
                          "single-valued, the `--app`/`--nest` shape: a harness input, never "
                          "product configuration. Reaches BOTH the availability probe and the "
                          "driver, so a box with two devices attached cannot probe one and "
                          "drive the other. Omit it to use whichever device is attached. "
                          "E2E_DEVICE_SERIAL is the env equivalent. "
                          "See testing.md § Default app and nest mode → Android's run venue.")
    parser.addoption("--adb-server", action="store", default=None,
                     help="The adb server an android run drives, as adb's own socket spec "
                          "with an explicit loopback host (e.g. --adb-server "
                          "tcp:127.0.0.1:18509): a server ALREADY RUNNING at the far end of "
                          "the venue's tunnel, reached as a pure client on every adb call. "
                          "Run-level, a harness input like --device-serial, and it reaches "
                          "the availability probe as well as the driver; with it set the "
                          "bridge forward and every nest take fixed ports the tunnel "
                          "carries (`just android-tunnel-spec`). Omit it for a local adb "
                          "server. E2E_ADB_SERVER is the env equivalent. "
                          "See testing.md § Default app and nest mode → Android's run venue.")
    parser.addoption("--strict-nest", action="store_true", default=False,
                     help="The `--strict-app` twin for the nest axis: turn every UNBUILT "
                          "mode-support skip into a failure, so a per-mode coverage claim is "
                          "falsifiable (testing.md § Default app and nest mode — eligibility is "
                          "classified exclusion, and a class that never shrinks is debt hiding in "
                          "an `s`). Only `helpers/nest_surface.py::mode_unbuilt` is affected: a "
                          "`declared_absence` (no virgin live box; instrumentation compiled out "
                          "of the image) and a `skip_environment` (no docker daemon, image "
                          "absent) still skip, because no harness work would make them run. "
                          "Every non-standalone run — flag or not — prints the mode-gate tally.")
    parser.addoption("--fail-on-skip", action="store_true", default=False,
                     help="GATE RUNS: red the run if any selected test SKIPPED. `pytest -m "
                          "cd_suite` exits 0 when every one of its tests skips on a missing "
                          "FAUNA_LIVE_* var, an unbuilt app binary, or an unreachable port — "
                          "so a release pipeline gating on the exit code can promote :latest "
                          "on the strength of `4 skipped`, which reads like success (the "
                          "convention-7 trap, one layer up). Deliberately broader than "
                          "--strict-app/--strict-nest, which each red only their own helper's "
                          "skips: a gate does not care WHY its suite failed to run. For "
                          "pipeline invocations, never the inner "
                          "loop — an ordinary sweep skips legitimately all the time.")
    parser.addoption("--max-run-secs", action="store", type=int, default=None,
                     metavar="SECS",
                     help="Wall-clock ceiling on the RUN, counted from its first test "
                          "(when the e2e lane is already held): once it has passed, the "
                          "next test does not start and the run exits 3, naming how far "
                          "it got. A running test is still bounded by the per-test "
                          "`timeout = 900`, so the lane is released within the ceiling "
                          "plus one test. For whole-suite runs (`just e2e-tier-23-check`), "
                          "whose ONLY other bound is the armed gate's own — a measuring "
                          "run launched on its own once held the one-wide e2e_other lane "
                          "for 13 h. Off by default: the inner loop names its tests.")
    parser.addoption("--feature", action="append", default=[], metavar="SLUG",
                     help="Select only tests carrying @pytest.mark.feature(SLUG). "
                          "Repeatable; combines with --app and --nest, which is the "
                          "way to re-witness one feature on one app in one nest mode "
                          "(feature-catalog.md § The marker). An unknown slug is a "
                          "usage error, not an empty run.")
    parser.addoption("--no-feature-ledger", action="store_true", default=False,
                     help="Do NOT record this run in docs/features/ledger/. The "
                          "ledger is on by default — a run that observed something "
                          "and did not record it is the failure the catalog exists "
                          "to prevent (feature-catalog.md § The ledger) — so this is "
                          "for a scratch run a session deliberately keeps out of the "
                          "record.")
    parser.addoption("--release-candidate", action="store_true", default=False,
                     help="Stamp this run's ledger lines with a BARE version "
                          "(`0.1.2 docker`) instead of `0.1.2-dev+<sha> <mode>`. "
                          "Honoured only after the plugin re-verifies the release "
                          "candidate preconditions itself — clean tree, HEAD equal to "
                          "the image's FAUNA_BUILD_COMMIT, a registry digest — so the "
                          "recipe is the convenient door and this is the gate.")
    parser.addoption("--nest-gates-json", action="store", default=None,
                     help="Write this run's nest-mode gate tally to this JSON path at "
                          "session end — the machine-readable twin of the "
                          "`nest-mode gates` terminal summary, carrying each excluded "
                          "test's repo-relative id and the feature slugs it witnesses, "
                          "plus everything the run did select. Consumed by the catalog's "
                          "mode-audit, which joins it to the coverage contracts to "
                          "answer which pages each exclusion class blanks "
                          "(feature-catalog.md § Cell semantics). A collect-only run "
                          "fills it: the classification is a collection-time verdict.")
    parser.addoption("--baseline-json", action="store", default=None,
                     help="Write per-test outcome + duration to this JSON path at session end "
                          "(schema: tests/e2e-unified/baselines/README.md). Consumed by "
                          "the interim apple regression gate "
                          "(apple-e2e-automation.md § Baseline discipline) until CI exists.")


def pytest_configure(config):
    """Push `--strict-app` into helpers/app_surface.py and `--nest` into
    helpers/nest_mode.py.

    Both are read from call sites with no `request` in scope — the
    unbuilt/absent/environment helpers run inside action classes, and
    `_make_nest` is called by ~17 fixtures — so this is the one place either
    flag can be handed over.
    """
    from helpers import app_surface, nest_mode as nest_mode_mod, nest_surface

    app_surface.set_strict_app(bool(config.getoption("strict_app")))
    nest_surface.set_strict_nest(bool(config.getoption("strict_nest")))
    # The artifact mode is read by `_build_app_via_just` and
    # `_build_app_config`, neither of which is handed the pytest `config`
    # (the first takes only a client name; the second is memoized behind the
    # driver cache) — so this is the one place the flag can be handed over.
    global _MACOS_ARTIFACT_MODE
    _MACOS_ARTIFACT_MODE = bool(config.getoption("macos_artifact"))
    # Resolve, validate, AND check the mode has a provider — all at configure
    # time, as a `UsageError` (pytest renders it as one clean line; letting a
    # `NestModeError` escape a hook prints an INTERNALERROR traceback instead).
    #
    # Refusing the whole RUN, rather than letting each nest-dependent test fail
    # at setup, is the point: `--nest docker` before slice 2 must stop
    # immediately, not produce a mostly-green run whose tier_3 half errored.
    try:
        mode = nest_mode_mod.resolve_nest_mode(config.getoption("nest"))
        nest_mode_mod.provider_for(mode)
    except nest_mode_mod.NestModeError as exc:
        raise pytest.UsageError(f"--nest: {exc}") from exc
    # The box declaration rides on the mode, validated here for the same
    # reason: `--live-box disposable` pointed at a box that is not a named
    # staging box must stop the RUN with the refusal, not deselect the
    # destructive residents quietly and pass green (the exact failure the flag
    # exists to close, from the other side).
    try:
        mode = nest_mode_mod.declare_box(mode, config.getoption("live_box"))
    except nest_mode_mod.NestModeError as exc:
        raise pytest.UsageError(f"--live-box: {exc}") from exc
    nest_mode_mod.set_run_mode(mode)

    # The android device axis, same shape and same reason: `_android_available()`
    # runs at collection and `_build_app_config` is memoized behind the driver
    # cache, so neither is handed the pytest `config` and this is the one place
    # the flag can be pushed into `helpers/android_device`. Resolved as a
    # UsageError here rather than as a per-test error later, exactly as the nest
    # mode above — an empty `--device-serial "$UNSET_VAR"` must stop the run,
    # not produce a green sweep against an arbitrary attached device.
    from helpers import android_device

    try:
        serial = android_device.resolve_device_serial(config.getoption("device_serial"))
    except android_device.AndroidDeviceError as exc:
        raise pytest.UsageError(f"--device-serial: {exc}") from exc
    android_device.set_device_serial(serial)
    # And the adb server that device hangs off, pushed in beside it for the
    # same reason: the probe, the launch config and the nest providers' port
    # choice all read it with no `config` in reach.
    try:
        adb_server = android_device.resolve_adb_server(config.getoption("adb_server"))
    except android_device.AndroidDeviceError as exc:
        raise pytest.UsageError(f"--adb-server: {exc}") from exc
    android_device.set_adb_server(adb_server)


def pytest_generate_tests(metafunc):
    """Stamp the nest mode into the ids of tests that actually depend on it.

    `test_x[tui-standalone]` vs `[tui-docker]` vs `[tui-live]` — per-mode
    distinguishability in logs, baselines, and flake history with zero file
    duplication (`testing.md:44`).

    Only tests whose fixture closure reaches `nest_mode` are stamped. A tier_1
    or tier_2 test that never touches a nest gets no stamp, because a mode is a
    harness input "meaningful only within tiers 3-4" (`testing.md:42`) — pinning
    a mode onto a test whose outcome cannot depend on it would assert something
    false, and would churn every node id in the suite for no information.

    **The DEFAULT mode is deliberately not stamped** — `[tui]` means standalone,
    `[tui-docker]` means docker (exactly `testing.md:44`'s illustration). The
    design record's `[tui-standalone]` form was measured and rejected here, for
    two reasons it could not have known:

      * It renames ~1400 node ids on the default path, including 1001 of the
        1936 keys in `baselines/apple-baseline.json`. That gate classifies a
        changed id as `added` + `removed`, never as `NEW RED`, so a genuine
        apple regression landing in the same window would be reported as a
        benign addition — a false green, and on a machine this one cannot
        regenerate from.
      * The stamp's position in an id is NOT stable: pytest orders parametrized
        args by fixture scope, so real collections produce `[standalone-ios]`,
        `[ios-standalone]` and `[ios-standalone-admin-…]` for different tests.
        The "distinguishable in baselines and flake history" benefit that
        justified stamping the default is therefore much weaker than assumed,
        while its cost is exact.

    Marking only the non-default modes keeps every default-path id stable
    forever and still distinguishes every mode that is actually in play.
    """
    if "nest_mode" not in metafunc.fixturenames:
        return
    from helpers import nest_mode as nest_mode_mod

    mode = nest_mode_mod.run_mode()
    if mode.is_standalone:
        return
    metafunc.parametrize(
        "nest_mode", [mode], ids=[mode.id], indirect=True, scope="session",
    )


def _report_frame_invariants(terminalreporter):
    """Say what convention 17's layer (b) saw this run — never a failure.

    The corpus is a file, and a file nobody opens is the same as no observation.
    The violating test almost always PASSES (the violation is latent — that is
    the thesis), and pytest swallows fixture prints on passing tests, so this
    summary is the only surface where the session that produced a violation
    meets it. It reports counts and names at most
    `frame_invariants.SUMMARY_DETAIL_CAP` offenders; the corpus holds the rest.
    """
    if _frame_tally is None:
        return
    tally, path = _frame_tally
    for line in tally.summary(path):
        terminalreporter.write_line(line)


def _report_foreground_takes(terminalreporter):
    """Say which windows tests still took the desktop's foreground through a
    bridge fallback (`helpers/foreground_takes.py`) — never a failure."""
    if _foreground_tally is None:
        return
    tally, path = _foreground_tally
    for line in tally.summary(path):
        terminalreporter.write_line(line)


def _report_live_reap(terminalreporter):
    """Say what the live reap did (`helpers/live_accounts.py`) — never a failure.

    The reap runs in the live nest's fixture finalizer, whose prints pytest
    swallows on a passing test, so this summary is the only surface where the
    `[live] reaped N` line and any RESIDUE warning reach the run's log."""
    from helpers import live_accounts

    for line in live_accounts.drain_report():
        terminalreporter.write_line(line)


def pytest_terminal_summary(terminalreporter, exitstatus, config):
    """Print the unbuilt-surface tally — the ratchet, printed every run.

    This is the number that makes a coverage claim falsifiable: without it, an
    unbuilt-surface skip disappears into the summary line's `N skipped` beside
    genuinely-absent and environment skips, and nobody can tell which app is
    actually covered. Printed whether or not `--strict-app` is set, because the
    point is that the number is *visible by default* and can only go down.
    """
    from helpers import app_surface

    # The nest axis reports first and unconditionally — the app tally below
    # returns early when it has nothing, and a reporter hidden behind another
    # reporter's early return is a reporter that never runs.
    _report_nest_mode_gates(terminalreporter, config)
    _report_ws_rpc_reconnects(terminalreporter)
    _report_frame_invariants(terminalreporter)
    _report_foreground_takes(terminalreporter)
    _report_live_reap(terminalreporter)

    violation = _fail_on_skip_violation(
        _skip_reasons, bool(config.getoption("fail_on_skip"))
    )
    if violation:
        terminalreporter.write_sep("=", "gate incomplete (--fail-on-skip)", red=True)
        for line in violation.splitlines():
            terminalreporter.write_line(line)

    hits = app_surface.unbuilt_hits()
    if not hits:
        return
    per_app: dict[str, list[tuple[str, str]]] = {}
    for app, surface, detail in hits:
        per_app.setdefault(app, []).append((surface, detail))

    strict = config.getoption("strict_app")
    verb = "FAILED (--strict-app)" if strict else "skipped"
    terminalreporter.write_sep("=", f"unbuilt app surfaces: {len(hits)} test(s) {verb}")
    for app in sorted(per_app):
        entries = per_app[app]
        terminalreporter.write_line(f"{app}: {len(entries)}")
        seen: set[str] = set()
        for surface, detail in entries:
            if surface in seen:
                continue
            seen.add(surface)
            n = sum(1 for s, _ in entries if s == surface)
            suffix = f" (x{n})" if n > 1 else ""
            terminalreporter.write_line(f"  - {surface}{suffix}")
    if not strict:
        terminalreporter.write_line(
            "These reported `s`, not a failure. Re-run with --strict-app to make "
            "them red (testing.md convention 7)."
        )


def _report_ws_rpc_reconnects(terminalreporter):
    """Print the WS-RPC reconnect tally — the third axis of "what did this run
    quietly paper over".

    The harness's WS-RPC clients are cached for the whole pytest process
    (`tests/api/ws_api.py`), so between calls they sit idle far past the
    nest's 60 s liveness window and get reaped exactly as designed. The client
    now replaces such a socket *before* sending (`clients/_ws_rpc_core.py`),
    which is correct and routine — but a reconnect nobody counts is a
    reconnect that can absorb a systematic nest-side drop. So the number is
    printed, always, and the two reasons are kept apart:

    * ``idle`` — the socket was parked past the reconnect floor. Expected on
      any long sweep; the count roughly tracks how much wall time passes
      between one actor's ``ws_api`` calls.
    * ``send-failed`` — a send raised on a socket the floor had *just*
      cleared. Nothing reached the nest so it was re-sent safely, but this
      should be rare; a run with many of them is evidence that something is
      dropping established connections, which is a product question.

    Silent when nothing reconnected, so the ordinary inner loop never sees it.
    """
    from clients import _ws_rpc_core

    events = _ws_rpc_core.reconnects()
    if not events:
        return
    idle = [e for e in events if e.reason == "idle"]
    send_failed = [e for e in events if e.reason == "send-failed"]
    terminalreporter.write_sep(
        "=", f"ws-rpc reconnects: {len(idle)} idle, {len(send_failed)} send-failed"
    )
    if idle:
        worst = max(e.idle_seconds for e in idle)
        terminalreporter.write_line(
            f"idle: {len(idle)} (longest park {worst:.0f}s) — routine; the "
            "cached client outlived the nest's liveness window between calls."
        )
    for event in send_failed:
        terminalreporter.write_line(
            f"send-failed: {event.kind} after {event.idle_seconds:.0f}s idle "
            "— re-sent on a fresh connection (nothing had reached the nest)."
        )
    if send_failed:
        terminalreporter.write_line(
            "A send-failed reconnect is not a harness parking bug. If these "
            "are frequent, ask why established connections are dropping."
        )


def _report_nest_mode_gates(terminalreporter, config):
    """Print the mode-gate tally — the nest axis's twin of the ratchet above.

    Silent on the default path (standalone excludes nothing), so the inner loop
    never sees it. In a docker or live run it is the answer to "what did this
    sweep NOT cover, and which of the ratified classes was it" — without it, a
    deselected test is indistinguishable from a test that never existed, and
    "runs in all modes" stops being falsifiable.
    """
    from helpers import feature_ledger, nest_surface

    # Reported first and apart from the gate tally below, because these tests
    # RAN — they were excluded from the LEDGER, not from the suite, and folding
    # them into a line that says "N test(s) excluded" would misread as a
    # deselection (class (7), testing.md § Default app and nest mode).
    refusals = feature_ledger.refusals()
    if refusals:
        by_reason: dict[str, int] = {}
        for _test_id, reason in refusals:
            by_reason[reason] = by_reason.get(reason, 0) + 1
        terminalreporter.write_sep(
            "=", f"feature ledger: {len(refusals)} outcome(s) observed but not recorded")
        for reason, n in sorted(by_reason.items(), key=lambda kv: -kv[1]):
            terminalreporter.write_line(f"  - {reason}{f' (x{n})' if n > 1 else ''}")

    hits = nest_surface.gate_hits()
    if not hits:
        return
    per_class: dict[str, list[tuple[str, str]]] = {}
    for hit in hits:
        per_class.setdefault(hit.klass, []).append((hit.test_id, hit.reason))

    mode_name = hits[0].mode
    strict = config.getoption("strict_nest")
    terminalreporter.write_sep(
        "=", f"nest-mode gates ({mode_name}): {len(hits)} test(s) excluded")
    for klass in sorted(per_class):
        entries = per_class[klass]
        terminalreporter.write_line(f"{klass}: {len(entries)}")
        # Group by reason, not by test: the reason is the class's shape, and one
        # cause routinely covers dozens of tests.
        by_reason: dict[str, int] = {}
        for _test_id, reason in entries:
            by_reason[reason] = by_reason.get(reason, 0) + 1
        for reason, n in sorted(by_reason.items(), key=lambda kv: -kv[1]):
            suffix = f" (x{n})" if n > 1 else ""
            terminalreporter.write_line(f"  - {reason}{suffix}")
    if not strict and nest_surface.MODE_UNBUILT in per_class:
        terminalreporter.write_line(
            "The mode_unbuilt entries reported `s`, not a failure. Re-run with "
            "--strict-nest to make them red (testing.md § Default app and nest mode)."
        )


# Accumulates {nodeid: {"outcome": ..., "duration": ...}} for --baseline-json.
# Populated regardless of the flag (cheap); only written to disk if it's set.
_baseline_results = {}

# Accumulates {nodeid: reason} for --fail-on-skip. Populated regardless of the
# flag (cheap); only consulted when it's set.
_skip_reasons = {}


def _skip_reason(report) -> str:
    """The human reason a report skipped, or a placeholder.

    A skip's `longrepr` is the `(path, lineno, reason)` triple pytest renders in
    the short summary; the reason is the only part worth quoting back, and it
    already carries the `skipif` message (which env var, which unbuilt surface).
    """
    longrepr = getattr(report, "longrepr", None)
    if isinstance(longrepr, tuple) and len(longrepr) == 3:
        return str(longrepr[2])
    return str(longrepr) if longrepr else "(no reason recorded)"


def _fail_on_skip_violation(skipped, enabled):
    """Return the gate-failure message when `--fail-on-skip` must red the run.

    `skipped` is {nodeid: reason}. A gate run's whole claim is "this suite
    executed green against the deployed artifact", so a skip refutes the claim
    exactly as a failure does — but reports `s` and exits 0. Naming every
    skipped test AND its reason is the point: a gate that reds without saying
    what failed to run just moves the mystery one layer out.
    """
    if not enabled or not skipped:
        return None
    lines = [
        f"--fail-on-skip: {len(skipped)} selected test(s) SKIPPED — a gate run "
        "proves nothing when its suite does not execute.",
    ]
    for nodeid in sorted(skipped):
        lines.append(f"  - {nodeid}: {skipped[nodeid]}")
    return "\n".join(lines)


# The catalog's run-time context, resolved once in `pytest_configure` so the per-report
# hook does no git or docker work: {"version", "commit", "nest_mode", "image_digest",
# "stamp", "features": {nodeid: [slug, ...]}}.
_feature_run: dict = {}

# Every item that survived collection, in the repo-relative spelling the catalog's
# contracts cite. The `--nest-gates-json` audit's second half (helpers/nest_surface.py
# `gates_payload`); populated for every run because it is one set insert per item.
_selected_repo_ids: set = set()


def _feature_ledger_note(report, outcome):
    """Record one report against every feature the test witnesses, per app.

    This rides `pytest_runtest_logreport` rather than a fixture because the outcome is
    only known here, and because the ledger must see errors and skips — the shapes a
    fixture never returns from. `teardown` is included so a test whose body passed but
    whose teardown blew up is recorded as the error it was.
    """
    if not _feature_run:
        return
    slugs = _feature_run["features"].get(report.nodeid)
    if not slugs:
        return
    from helpers import feature_ledger

    if outcome is None:
        return
    # A test that brought its own nest artifact is a witness of THAT artifact, so
    # its record carries the booted container's provenance — never the run's —
    # and is declined, tallied, when the test observed no single nameable
    # artifact (feature-catalog.md § The ledger; helpers/feature_ledger).
    own_artifact = feature_ledger.own_artifact_this_test()
    if own_artifact:
        provenance, why = feature_ledger.own_artifact_resolution()
        if provenance is None:
            feature_ledger.note_refusal(
                report.nodeid,
                f"the test {own_artifact}, but {why} — the record would name no "
                f"artifact (feature-catalog.md § The ledger)",
            )
            return
        version, commit = provenance["version"], provenance["commit"]
        image_digest, nest_mode = provenance["image_digest"], "docker"
        # The image commit's own place in main's history, asked once when the
        # container answered health — never this checkout's (§ The ledger,
        # *Rebased commits*).
        base = provenance.get("base")
        # Never dirty: the image is a built artifact, and whatever sits uncommitted
        # in THIS checkout is not in it (§ The ledger, *Dirty trees*).
        dirty = False
        record_stamp = feature_ledger.stamp(version, commit, nest_mode,
                                            release_candidate=False, dirty=dirty)
    else:
        version, commit = _feature_run["version"], _feature_run["commit"]
        base = _feature_run.get("base")
        image_digest, nest_mode = _feature_run["image_digest"], _feature_run["nest_mode"]
        dirty = _feature_run["dirty"]
        record_stamp = _feature_run["stamp"]
    skip_class = feature_ledger.skip_class_this_test()
    # An environment skip says nothing about the feature — the previous ledger line
    # stands rather than being overwritten with a skip (§ Cell semantics).
    if outcome == "skipped" and skip_class == feature_ledger.ENVIRONMENT:
        return
    if outcome == "skipped" and skip_class is None:
        return  # an undeclared skip is not a witness of anything either

    apps = feature_ledger.apps_this_test() or {feature_ledger.NEST}
    node_id = _feature_run["node_ids"].get(report.nodeid) or report.nodeid
    for app in sorted(apps):
        for slug in slugs:
            feature_ledger.record(
                app, slug, node_id,
                outcome=outcome,
                skip_class=skip_class if outcome == "skipped" else None,
                version=version, commit=commit, base=base, image_digest=image_digest,
                nest_mode=nest_mode, record_stamp=record_stamp, dirty=dirty)


# Monotonic stamp of the run's first test setup — the `--max-run-secs` origin —
# and how many tests have started since.
_run_started_at: float | None = None
_run_tests_started = 0


def _run_ceiling_breach(started, now, max_secs, done, total):
    """The `--max-run-secs` abort message when the ceiling has passed, else None.

    Counted from the first test rather than from launch: collection and the
    slot queue hold no e2e lane, and the lane's occupancy is what the ceiling
    bounds."""
    if not max_secs or started is None or now - started < max_secs:
        return None
    return (
        f"[RUN CEILING] --max-run-secs {max_secs} passed after {done} of {total} "
        f"selected tests ({int(now - started)} s since the first) — the rest are "
        f"NOT run, so this is no complete verdict; aborting to release the e2e lane"
    )


def pytest_runtest_setup(item):
    """Clear the per-test attribution slots before each test (helpers/feature_ledger).

    Then set the two slots that are properties of the ITEM rather than of anything
    the test does: its real fixture closure, and whether it brings its own nest
    artifact. Both are read off the closure here, before the test runs, because
    that is the only moment the item is in scope and the answer cannot depend on
    how far the test got — a self-contained module that errors in setup is
    recorded (or declined) against its own artifact for exactly the same reason a
    passing one is.

    First, though, the `--max-run-secs` ceiling: a run past it starts no more tests.
    """
    global _run_started_at, _run_tests_started
    now = time.monotonic()
    if _run_started_at is None:
        _run_started_at = now
    breach = _run_ceiling_breach(
        _run_started_at, now, item.config.getoption("max_run_secs"),
        _run_tests_started, len(item.session.items),
    )
    if breach:
        pytest.exit(breach, returncode=3)
    _run_tests_started += 1

    from helpers import feature_ledger, nest_surface
    from helpers.fixture_closure import real_fixture_closure

    feature_ledger.reset_test_state()
    closure = real_fixture_closure(item)
    feature_ledger.note_closure(closure)
    own_image = nest_surface.OWN_IMAGE_FIXTURES.intersection(closure)
    if own_image:
        feature_ledger.note_own_artifact(
            f"boots {nest_surface.OWN_IMAGE_TAG} itself, via "
            f"{', '.join(sorted(own_image))}"
        )


@pytest.hookimpl(hookwrapper=True)
def pytest_fixture_setup(fixturedef, request):
    """Bracket every fixture's setup so `feature_ledger.note_container` can attach a
    booted artifact to the fixture whose setup observed it.

    The `tests/platform/docker/` container fixtures are mostly module-scoped, so
    a per-test slot would reach only the first test of each module; keyed by
    fixture, the artifact is found through every later test's fixture closure
    for as long as that fixture instance is alive (helpers/feature_ledger).
    """
    from helpers import feature_ledger

    feature_ledger.enter_fixture_setup(fixturedef.argname)
    try:
        yield
    finally:
        feature_ledger.leave_fixture_setup(fixturedef.argname)


def pytest_runtest_logreport(report):
    if report.outcome == "skipped" and getattr(report, "wasxfail", None) is None:
        _skip_reasons.setdefault(report.nodeid, _skip_reason(report))
    outcome = None
    if report.when == "call":
        if getattr(report, "wasxfail", None) is not None:
            outcome = "xfailed" if report.outcome == "skipped" else "xpassed"
        else:
            outcome = report.outcome
    elif report.when == "setup":
        if report.outcome in ("skipped", "failed"):
            outcome = "skipped" if report.outcome == "skipped" else "error"
    elif report.when == "teardown":
        if report.outcome == "failed" and report.nodeid not in _baseline_results:
            outcome = "error"
    if outcome is not None:
        _baseline_results[report.nodeid] = {
            "outcome": outcome,
            "duration": round(report.duration, 3),
        }
    if outcome == "error":
        _print_live_reason(report, "ERROR")
    elif outcome == "failed":
        _print_live_reason(report, "FAILED")
    if outcome is not None or report.when == "teardown":
        _feature_ledger_note(report, outcome)


def _print_live_reason(report, label):
    """Print the crash line of an ERROR or a FAILED the moment it is recorded.

    pytest holds every traceback until the end-of-run summary. That is fine for a
    ten-minute run and useless for a long one: the 2026-08-30 docker sweep spent
    **17 hours** producing 135 fixture errors whose reason could not be read until
    it finished, so three consecutive sessions bisected the cascade as a black box
    — re-running 10-to-20-minute cuts to guess at a string the run already had in
    hand. An ERROR is a *precondition* failing, and a precondition that fails
    silently for hours is exactly the diagnose-itself gap conventions point 6
    closes for assertions; this is the same rule applied to a run's own clock.

    **FAILEDs joined ERRORs on 2026-09-21**, when the "already reported in the
    summary" assumption behind ERRORs-only turned out to hold only where a run
    reaches its summary. A held traceback is lost by ANY abrupt end — but on
    Windows that end is the ordinary one: pytest-timeout has no `signal` method
    there, so its `thread` method dumps stacks and `os._exit`s the process, and
    neither `pytest_sessionfinish` nor the terminal reporter's failure summary
    ever runs. An 11-file windows run printed `F` twice, wedged in a third and
    died at 900 s with no summary and no counts; neither failure reproduced in
    three isolated re-runs, so both causes are permanently unrecoverable. The two labels stay distinct because the
    diagnoses are: an ERROR is a precondition, a FAILED is the subject itself.

    One truncated line per report, terminal-only, never fatal — a bookkeeping
    failure must not turn a run that produced real results red (the rule
    `_write_feature_ledger` already follows).
    """
    try:
        crash = getattr(report.longrepr, "reprcrash", None)
        message = getattr(crash, "message", None) or str(report.longrepr)
        first = message.strip().splitlines()[0] if message.strip() else "<no reason>"
        if len(first) > 200:
            first = first[:197] + "..."
        print(f"\n[e2e] {label} {report.nodeid} :: {first}", flush=True)
    except Exception as exc:  # noqa: BLE001 - diagnostics never fail a run
        print(
            f"\n[e2e] {label} {report.nodeid} :: <reason unavailable: {exc!r}>",
            flush=True,
        )


def _write_feature_ledger(session):
    """Merge what this run observed into `docs/features/ledger/<app>.json`.

    Written by the run, committed by the session — the ui-actual discipline
    (feature-catalog.md § The ledger). Never fatal: a run that produced real results
    must not be turned red by a bookkeeping failure, so a broken write is reported and
    the exit code is left to the tests.
    """
    if not _feature_run:
        return
    from helpers import feature_ledger
    try:
        written = feature_ledger.write()
    except OSError as exc:
        print(f"\nfeature ledger: NOT written ({exc})")
        return
    if written:
        names = ", ".join(sorted(p.name for p in written))
        print(f"\nfeature ledger: updated {names} — review the diff and commit it "
              f"with your work (run stamp: {_feature_run['stamp']}).")
        if _feature_run["dirty"]:
            # The harness says a word: the dirty stamp was first caught by eye,
            # re-reading a diff, because nothing here had mentioned the tree.
            dirt = _feature_run["dirt"]
            shown = ("git could not read the tree" if dirt is None
                     else "; ".join(dirt[:6]) + (f"; … {len(dirt) - 6} more"
                                                 if len(dirt) > 6 else ""))
            print(f"feature ledger: the tree was DIRTY at collection, so every record "
                  f"and cell this run wrote is marked `.dirty` — commit "
                  f"{(_feature_run['commit'] or '?')[:12]} is a lower bound on the code "
                  f"it saw, and a run from a clean tree supersedes it "
                  f"(feature-catalog.md § The ledger). Dirt: {shown}")
        # The other word the harness says: HEAD being ahead of main is the
        # precondition of an orphaned `commit` (the pre-merge rebase replaces it
        # whenever main moved during the run), and unlike the orphan itself it IS
        # knowable here. `base` is the sha that will still resolve either way.
        commit, base = _feature_run.get("commit") or "", _feature_run.get("base")
        if base is None:
            print("feature ledger: this checkout knows no `origin/main`, so every "
                  "record's `base` is unknown (null) — the commit it names may not "
                  "resolve on main once rebased (feature-catalog.md § The ledger, "
                  "*Rebased commits*).")
        elif commit and base != commit:
            print(f"feature ledger: HEAD is ahead of origin/main — the records name "
                  f"commit {commit[:12]}, which the pre-merge rebase will replace if "
                  f"main moves before this lands; their `base` {base[:12]} is the "
                  f"main commit beneath it and always resolves (feature-catalog.md § "
                  f"The ledger, *Rebased commits*).")

    # A candidate run is also a RELEASE record: the table is what `just release-tag`
    # reads, and it has to say this artifact was tested at this commit whatever the
    # tests themselves reported. Pre-1.0 the gate asks that the run happened and was
    # recorded, not that it was green everywhere (§ Goal) — so the row lands on a red
    # run too, and the matrix beside it is the honest label on the release.
    if not _feature_run.get("candidate"):
        return
    try:
        path = feature_ledger.record_release(
            version=_feature_run["version"], commit=_feature_run["commit"],
            image_digest=_feature_run["image_digest"], image=_feature_run.get("image"))
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"\nrelease table: NOT written ({exc}) — `just release-tag` will refuse "
              f"until this run is recorded")
        return
    print(f"release table: recorded {_feature_run['version']} @ "
          f"{_feature_run['commit'][:12]} / {_feature_run['image_digest']} in "
          f"{path.name} — commit it, then `just release-tag`.")


def _write_nest_gates(session):
    """Write `--nest-gates-json` if the flag is set; no-op otherwise.

    Written at session end rather than at collection so a runtime declaration
    (`nest_surface.mode_unbuilt` and friends, which fire from action code long
    after collection) lands in the same file as the collection-time verdicts.
    A `--collect-only` run therefore gets the collection half and nothing else,
    which is exactly right for the audit: the classification IS a collection
    verdict, and no container needs to exist to compute it.
    """
    path = session.config.getoption("nest_gates_json")
    if not path:
        return
    from helpers import nest_mode as nest_mode_mod, nest_surface

    payload = nest_surface.gates_payload(
        mode=nest_mode_mod.run_mode().name, collected=_selected_repo_ids,
        selection=sys.argv[1:])
    out = Path(path)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def pytest_sessionfinish(session, exitstatus):
    """Write --baseline-json if the flag is set; no-op otherwise.

    Kept deliberately dumb (outcome + duration only, no markers/tiers) — the
    interim apple regression gate's own orchestration owns merging clients,
    stamping HEAD sha, and diffing against the checked-in baseline. This hook's
    only job is "what happened in THIS pytest process".

    Also releases the machine-wide 'e2e' slot (taken in pytest_collection_finish)
    so a queued sibling run starts immediately — the kernel flock would release
    on process exit anyway; this just hands it over before teardown reporting.

    And enforces `--fail-on-skip`: the exit code is the only thing a release
    pipeline reads, so the flag has to move THAT, not just print a banner. The
    message itself is written by `pytest_terminal_summary` above.
    """
    _release_e2e_slot()

    if _fail_on_skip_violation(_skip_reasons, bool(session.config.getoption("fail_on_skip"))):
        session.exitstatus = pytest.ExitCode.TESTS_FAILED

    _write_feature_ledger(session)
    _write_nest_gates(session)

    baseline_path = session.config.getoption("baseline_json")
    if not baseline_path:
        return
    counts = {}
    for result in _baseline_results.values():
        counts[result["outcome"]] = counts.get(result["outcome"], 0) + 1
    payload = {"counts": counts, "tests": _baseline_results}
    path = Path(baseline_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def pytest_sessionstart(session):
    """Session bootstrap: opt every locally-spawned binary nest out of TLS.

    **TLS opt-out (all platforms).** A fauna-nest now serves an always-live
    self-signed HTTPS floor unconditionally — with or without a configured domain
    (tls-certificates.md § A; domains-and-tls-bootstrap.md). That is correct for
    every real deployment, but the tier_3 binary-e2e suite connects to nests over
    plain `http://` from dozens of scattered call sites across all 6 client
    platforms (many self-spawn their own nest). Rather than sweep the whole suite
    onto https in one cross-platform change, the suite opts out here: every nest
    that inherits this process's env (the central `start_nest` helper *and* the
    per-platform self-spawns) reads `FAUNA_INSECURE_DISABLE_TLS` and serves plain
    HTTP. The TLS floor itself is covered by the nest's unit tests + the tier_4
    Docker suite (which runs real https). `setdefault` lets a developer force the
    https path by pre-setting the var to `0`. Containers (tier_4) and the remote
    live box (`live_box`) don't inherit this env, so they are unaffected.

    (The former macOS AutomationMode wedge gate was removed with the XCUITest
    bridge at the Phase-3 cutover — apple e2e is now fully in-process, so there is
    no AutomationMode to wedge.)
    """
    # Plain-HTTP for tier_3 binary nests (see the TLS opt-out note above). Set
    # before any nest spawns; inherited by every Popen that doesn't pass a clean
    # env. tier_4 containers / the remote live box don't inherit it.
    os.environ.setdefault("FAUNA_INSECURE_DISABLE_TLS", "1")


# ── Machine-wide e2e slot (the fleet's build-slot tool, the e2e app lanes) ────
# A full e2e run spawns nests + browsers + client apps; N concurrent runs from N
# sessions thrash the shared machine (and the pytest-timeout bounds start
# flaking under the load they themselves create). Every pytest run that selects
# any tier_2/3/4 test takes one slot of its app lane — 'e2e_tui' for a tui-only
# or app-less run, 'e2e_other' otherwise (`_run_app_set`) — for the whole
# session — acquired here in conftest (not in the `just` recipes) so direct
# `pytest tests/e2e-unified/...` invocations queue too. tier_1-only runs are
# in-process and skip it. Loud + bounded like every machine-wide lock (point 9;
# sibling: helpers/live_box_lock.py): waiters name the holders once a minute and
# fail with that diagnosis after FAUNA_SLOT_TIMEOUT (default 7200s). Reentrancy
# via FAUNA_SLOT_HELD_E2E means an externally slot-wrapped pytest (or a nested
# pytest) runs directly instead of deadlocking against its parent's slot.
#: (module, [(fd, pool) granted, in acquisition order], the hold) while held;
#: the hold is `_new_hold`'s dict, handed to the lane usage record at release.
_E2E_SLOT = None

#: This harness's epoch, sent with every e2e lane acquire: the machine's
#: build-slot copy refuses a lane to a harness below its MIN_E2E_HARNESS_EPOCH, so a
#: checkout that has not rebased cannot hold a lane running old orchestration.
#: Bump it — and `scripts/build-slot.py::MIN_E2E_HARNESS_EPOCH` in the same
#: commit — whenever lane policy that lives here changes (the warm pass, refuse
#: mode, the lane choice). 1: refuse mode and the `fauna-ffi` cdylib warm job.
E2E_HARNESS_EPOCH = 1


def _build_slot_module():
    """Import the machine-wide build-slot tool (hyphenated filename → importlib by path).

    Machine-policy resolution: the slot pool's widths, floors, and reservation arithmetic are
    machine-wide policy, enforced from the MAIN checkout's copy — so load this
    checkout's copy first, ask ITS resolver which copy the machine runs
    (`_machine_policy_script`: None means "this one" — identical bytes, a
    redirected FAUNA_SLOT_DIR namespace, or FAUNA_SLOT_POLICY=self), and import
    what it names. Mirrored by
    test_build_slot.py::test_the_library_resolution_loads_the_machine_copy.

    Returns None if the slot script does not exist — it is fleet-only tooling
    that does not ship, and a solo checkout has no sibling e2e runs to queue
    against."""
    local_path = Path(__file__).resolve().parents[2] / "scripts" / "build-slot.py"
    if not local_path.exists():
        return None

    import importlib.util

    def _load(path):
        spec = importlib.util.spec_from_file_location("fauna_build_slot", str(path))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod

    local = _load(local_path)
    target = local._machine_policy_script()
    return _load(target) if target else local


def _refuse_drafts_window_collision(session) -> None:
    """Refuse a selection that both re-times and relies on the drafts debounce.

    `drafts_autosave_window_ms` is session-wide (the drivers are session-scoped),
    so it re-times EVERY app launched in the run. A test marked
    `drafts_production_window` asserts a draft the debounce itself must land, so
    a lengthened window turns it red — not flakily, but every time, and the red
    would be recorded against the app in the feature ledger as if the product
    had regressed. Refusing at collection is the cheap, loud alternative; it
    runs before the e2e slot is acquired, so a bad selection costs nothing.
    """
    if _selected_drafts_autosave_window_ms(session) is None:
        return
    clashing = sorted(
        {
            item.nodeid.split("::")[0]
            for item in session.items
            if item.get_closest_marker("drafts_production_window") is not None
        }
    )
    if not clashing:
        return
    raise pytest.UsageError(
        "drafts_autosave_window_ms re-times the drafts debounce for every app in "
        "the session, but this selection also contains tests that need the "
        "production window to fire:\n  "
        + "\n  ".join(clashing)
        + "\nRun the re-timed test in its own invocation."
    )


#: The app names that are also pytest markers (pytest.ini) — a non-parametrized
#: app-specific suite names its app this way.
_APP_MARKERS = frozenset({"web", "android", "ios", "macos", "windows", "linux", "tui"})

#: Prebuild jobs the warm pass skips: they are not freshness-gated compiles
#: whose result a second call re-verifies for free, but one-shot session side
#: effects (loading a cdylib into this process, creating and booting a
#: simulator, reassembling the apple xcframework) that must happen exactly once,
#: inside the hold.
_NOT_PREWARMED_BUILDERS = frozenset({
    "_ensure_fauna_ffi_loaded",
    "_get_ios_setup",
    "_restore_full_apple_ffi",
    "_ensure_macos_pkg_built",
})

#: Where the warm pass runs: the platforms on which every prebuild path gates
#: freshness OUTSIDE its `{{slot_build}}`, so the in-hold re-check of a warm
#: tree takes no build slot. Elsewhere a warm re-check may still queue for `build`, and
#: the warm pass would buy a second wait instead of removing one. `win32` joined
#: once its last ungated path, `tui-debug`'s extensionless `--target`, was fixed.
_PREWARM_PLATFORMS = frozenset({"linux", "win32"})


def _run_app_set(session) -> set:
    """Every app this run can launch — what decides its e2e lane
    (`build-slot.py::e2e_app_pool`). The prebuild's scan, plus the apps its
    fixture-keyed app builds name, the app markers of non-parametrized suites,
    and the web SPA when the run drives it."""
    requested: set = set()
    for item in session.items:
        requested.update(_real_fixture_closure(item))
    apps = set(_prebuild_app_set(session, requested))
    apps.update(
        args[0]
        for fixture_name, builder, args in _PREBUILD_BY_FIXTURE
        if builder == "_ensure_app_built" and fixture_name in requested
    )
    if "pkg_path" in requested:
        apps.add("macos")
    available = set(get_available_apps())
    for item in session.items:
        # An app-parametrized item launches its parameter; its markers only say
        # which apps the test applies to (a `[tui]` item marked `linux`, `web`
        # must not send a tui run to `e2e_other`).
        if not _item_param_apps(item, available):
            apps.update({m.name for m in item.iter_markers()} & _APP_MARKERS)
        if "[web]" in item.nodeid or "web" in item.nodeid.partition("[")[2]:
            apps.add("web")
    return apps


def _prewarm_before_slot(mod) -> bool:
    """Whether to run the warm prebuild pass before taking the e2e slot: on a
    `_PREWARM_PLATFORMS` machine, and only for a tree holding no slot yet — a
    run already inside the e2e family (an explicit lane wrap, a nested pytest)
    has nothing to gain, and a build-held tree is refused at the acquire."""
    if sys.platform not in _PREWARM_PLATFORMS:
        return False
    if os.environ.get("FAUNA_SLOT_HELD_BUILD"):
        return False
    holds_e2e = getattr(mod, "_holds_e2e", None)
    if holds_e2e is not None:
        return not holds_e2e()
    return not os.environ.get("FAUNA_SLOT_HELD_E2E")


def _prewarm_prebuilds(session) -> None:
    """The warm pass: every compile this run needs, BEFORE the e2e slot, so a
    cold tree's `build` queue is waited out while no e2e slot sits idle.

    Then everything it built is FORGOTTEN, so the in-hold pass re-verifies it on
    disk. That second pass is the dataset lease: the pressure tier may evict this checkout's
    target while it queues for `e2e`, and only the in-hold pass is protected. On
    a warm tree the re-check is a stamp read and takes no slot; after an eviction
    it rebuilds, which is exactly what today's in-hold build would have cost.
    Failures are printed and kept memoized, so the in-hold pass replays them
    instead of compiling the same error twice."""
    global _WEB_SPA_BUILT, _WASM_PANIC_WITNESS_BUILT
    print(
        "[prebuild] warm pass — building before the e2e slot, so the slot is never "
        "held idle behind a build queue",
        flush=True,
    )
    try:
        _prebuild_web_spa(session)
    except Exception as exc:
        print(f"[prebuild] warm pass: web SPA build FAILED ({exc}); retried in the hold", flush=True)
    _prebuild_binaries(session, prewarm=True)
    for key, (kind, _value) in list(_BINARY_BUILD_MEMO.items()):
        if kind == "ok":
            del _BINARY_BUILD_MEMO[key]
    _WEB_SPA_BUILT = False
    _WASM_PANIC_WITNESS_BUILT = False


def pytest_collection_finish(session):
    """Warm the prebuilds, then acquire this run's machine-wide e2e lane, then
    re-check the prebuilds inside the hold — when this run drives real
    drivers/nests."""
    global _E2E_SLOT
    if session.config.option.collectonly:
        return
    _refuse_drafts_window_collision(session)
    heavy = {"tier_2", "tier_3", "tier_4"}
    if not any(
        {m.name for m in item.iter_markers()} & heavy for item in session.items
    ):
        _admit_tier_1_run()
        return
    mod = _build_slot_module()
    if mod is None:
        _prebuild_web_spa(session)
        _prebuild_binaries(session)
        return
    cmd = [os.path.basename(sys.argv[0] or "pytest")] + sys.argv[1:]
    # The app lanes: a tui-only (or app-less) run takes `e2e_tui`, anything launching
    # another app `e2e_other`. getattr: the machine's policy copy may predate
    # the lanes for a while after this lands, and then the run takes the one
    # `e2e` pool, as before.
    apps = _run_app_set(session)
    app_pool_of = getattr(mod, "e2e_app_pool", None)
    e2e_pool = app_pool_of(apps) if app_pool_of is not None else "e2e"
    # An unbounded lane (build-slot.py § Pools → the unbounded lane; the
    # primary dev VM since 2026-10-02): granted at once, so nothing waits for it and refuse mode
    # below has nothing to protect. getattr: a machine copy predating the
    # predicate bounds every lane, as before.
    unbounded_of = getattr(mod, "lane_unbounded", None)
    lane_unbounded = unbounded_of is not None and bool(unbounded_of(e2e_pool))
    print(
        f"[e2e-slot] lane: {e2e_pool}{' (unbounded — no count admits here)' if lane_unbounded else ''}"
        f" — apps {sorted(apps) or ['none']}",
        file=sys.stderr,
        flush=True,
    )
    # The warm pass: compile before queueing for the slot, so a held e2e slot is
    # not spent waiting in the build queue.
    warmed = _prewarm_before_slot(mod)
    if warmed:
        _prewarm_prebuilds(session)
    # The long lane: a run whose heavy selection is long takes the 1-slot `e2e_long`
    # mutex BEFORE its `e2e` slot, so at most one long run occupies the pool
    # and the other slot turns over at short-run pace. getattr: the machine's
    # policy copy may predate the lane for a while after this lands, and then
    # every run is short, as before.
    heavy_tests = sum(
        1 for item in session.items if {m.name for m in item.iter_markers()} & heavy
    )
    lane_of = getattr(mod, "e2e_lane", None)
    long_lane = lane_of is not None and lane_of(heavy_tests) == "long"
    if long_lane:
        print(
            f"[e2e-slot] lane: e2e (long) — {heavy_tests} tier_2/3/4 tests selected "
            f"(>= {mod.LONG_LANE_MIN_HEAVY_TESTS}); taking the '{mod.E2E_LONG_POOL}' "
            f"mutex before the '{e2e_pool}' slot",
            file=sys.stderr,
            flush=True,
        )
        # Tag the slot notes so `build-slot.py --status` names the long holder.
        cmd = [f"{cmd[0]} [e2e long lane]"] + cmd[1:]
    hold = _new_hold(apps)
    held = _acquire_lanes(mod, e2e_pool, long_lane, cmd)
    _E2E_SLOT = (mod, held, hold)
    # No compile under a held lane: the in-hold
    # re-check runs in refuse mode, so a tree that is not warm after all (the
    # warm pass failed, an input moved during the lane wait, the target was
    # evicted) is REFUSED its `build` slot instead of compiling with the lane
    # idle behind the build queue. Then: release the lane, warm the tree
    # holding nothing, re-queue, re-check. Bounded; past the bound the run
    # builds in the hold as before (the lock must never make work impossible).
    if warmed and _refuse_mode_available(mod) and not lane_unbounded:
        for cycle in range(1, _INHOLD_REFUSE_RETRIES + 1):
            started = time.monotonic()
            try:
                refused = _prebuild_in_hold_refusing(session, mod)
            finally:
                hold["prebuild_s"] += time.monotonic() - started
            if not refused:
                return
            print(
                f"[e2e-slot] releasing the '{e2e_pool}' lane (cycle {cycle} of "
                f"{_INHOLD_REFUSE_RETRIES}): the in-hold re-check found the tree "
                f"NOT warm — it would have compiled {len(refused)} build(s) with the "
                f"lane idle behind the build queue: {'; '.join(r[:120] for r in refused)}. "
                "The refusal built nothing and invalidated nothing (no artifact, "
                "stamp or sibling build is touched); this run hands the lane to the "
                "next queued run, compiles those builds holding no lane, then "
                "re-queues for the lane and re-checks.",
                file=sys.stderr,
                flush=True,
            )
            _E2E_SLOT = None
            _release_lanes(mod, held, hold)
            _forget_prebuilds_for_retry()
            _prewarm_prebuilds(session)
            hold = _new_hold(apps)
            held = _acquire_lanes(mod, e2e_pool, long_lane, cmd)
            _E2E_SLOT = (mod, held, hold)
        print(
            f"[e2e-slot] the tree was still not warm after {_INHOLD_REFUSE_RETRIES} "
            "release-and-warm cycles — building inside the hold this once",
            file=sys.stderr,
            flush=True,
        )
    started = time.monotonic()
    try:
        _prebuild_web_spa(session)
        _prebuild_binaries(session)
    finally:
        hold["prebuild_s"] += time.monotonic() - started


def _admit_tier_1_run() -> None:
    """A run selecting no tier_2/3/4 test takes the `tier_1` slot itself where
    the machine's slot copy self-admits test runs (`build-slot.py::
    admit_test_run`, macOS today) — so a session's bare tier_1 pytest takes its slot
    like a recipe's `{{slot_tier_1}}` does. Held in
    `_E2E_SLOT` and released at session end by the same path as a lane.
    getattr, as for every lane feature: a machine copy predating it takes
    nothing, as before."""
    global _E2E_SLOT
    mod = _build_slot_module()
    admit = getattr(mod, "admit_test_run", None) if mod is not None else None
    if admit is None:
        return
    cmd = [os.path.basename(sys.argv[0] or "pytest")] + sys.argv[1:]
    try:
        fd = admit(cmd)
    except Exception as exc:
        order_err = getattr(mod, "SlotOrderError", None)
        if order_err is not None and isinstance(exc, order_err):
            pytest.exit(str(exc), returncode=4)
        raise
    if fd is not None:
        print("[e2e-slot] tier_1-only run: took the 'tier_1' slot", file=sys.stderr, flush=True)
        _E2E_SLOT = (mod, [(fd, "tier_1")], None)


def _new_hold(apps) -> dict:
    """One lane hold's bookkeeping for its usage record: the run's app set and
    the wall seconds of the hold spent in the in-hold prebuild pass (the one
    place a held lane can wait for or hold `build`). A refuse-mode requeue is a
    new hold, with its own dict and its own usage line."""
    return {"apps": sorted(apps), "prebuild_s": 0.0}


def _release_e2e_slot() -> None:
    """Release whatever lanes this run still holds — the session-end hand-over
    (`pytest_sessionfinish`), through the same path, usage record included, as
    the refuse-mode loop's release."""
    global _E2E_SLOT
    if _E2E_SLOT is not None:
        mod, held, hold = _E2E_SLOT
        _E2E_SLOT = None
        _release_lanes(mod, held, hold)


#: How many times the in-hold re-check may find the tree cold, release the lane
#: and warm it again before the run builds inside the hold as it did before
#: 2026-09-29. Two is enough for the observed causes (a warm pass that failed on
#: a mid-edit tree, an input that moved during the lane wait); the bound exists
#: for a path that takes `build` on every call regardless of freshness, which
#: would otherwise cycle forever.
_INHOLD_REFUSE_RETRIES = 2


def _refuse_mode_available(mod) -> bool:
    """Whether the machine's slot copy knows refuse mode (`REFUSE_BUILD_ENV`,
    `SlotRefusedError`, `release_slot(fd, pool)` landed together). getattr, as
    for every lane feature: the policy copy may predate it for a while after it
    lands, and then the in-hold pass runs as before."""
    return getattr(mod, "REFUSE_BUILD_ENV", None) is not None and hasattr(mod, "SlotRefusedError")


def _acquire_lanes(mod, e2e_pool: str, long_lane: bool, cmd: list[str]) -> list[tuple[int, str]]:
    """Take this run's e2e lane (after the long-lane mutex when `long_lane`);
    returns the granted (fd, pool) pairs in acquisition order."""
    held: list[tuple[int, str]] = []
    # The harness epoch rides along wherever the machine's copy checks it;
    # getattr, as for every lane feature: a copy predating the check has no
    # `harness_epoch` parameter.
    epoch = (
        {"harness_epoch": E2E_HARNESS_EPOCH}
        if getattr(mod, "MIN_E2E_HARNESS_EPOCH", None) is not None
        else {}
    )
    try:
        if long_lane:
            long_fd = mod.acquire_slot(mod.E2E_LONG_POOL, cmd=cmd, **epoch)
            if long_fd is not None:
                held.append((long_fd, mod.E2E_LONG_POOL))
        fd = mod.acquire_slot(e2e_pool, cmd=cmd, **epoch)
        if fd is not None:
            held.append((fd, e2e_pool))
    except Exception as exc:
        _release_lanes(mod, held)
        # Cross-pool order violation (this run was wrapped in the BUILD pool —
        # build-slot.py's module docstring § Cross-pool order) or a stale
        # harness (StaleHarnessError, a subclass — rebase): exit cleanly
        # with the remedy instead of an INTERNALERROR traceback. getattr, not a
        # bare except clause, because the machine's policy copy may predate the
        # class for a while after this lands (frozen-interface transition).
        order_err = getattr(mod, "SlotOrderError", None)
        if order_err is not None and isinstance(exc, order_err):
            pytest.exit(str(exc), returncode=4)
        raise
    return held


def _release_lanes(mod, held: list[tuple[int, str]], hold: dict | None = None) -> None:
    """Release the lanes `_acquire_lanes` granted — the e2e lane first, then the
    long-lane mutex — clearing the held markers with the lock (the pool-aware
    `release_slot`) wherever the machine's copy has it, so the re-acquire in
    the refuse-mode loop is not short-circuited as reentrant.

    Each release first hands the hold to the lane usage record:
    `log_lane_hold` writes one `kind: usage` line for a lane fd and nothing for
    the long-lane mutex. getattr, as for every lane feature: a machine copy
    predating the record writes nothing, and a failure never blocks a release."""
    log_hold = getattr(mod, "log_lane_hold", None)
    for fd, pool in reversed(held):
        if log_hold is not None and hold is not None:
            try:
                log_hold(fd, hold["apps"], inhold_prebuild_s=hold["prebuild_s"])
            except Exception:
                pass
        if _refuse_mode_available(mod):
            mod.release_slot(fd, pool)
        else:
            mod.release_slot(fd)


def _prebuild_in_hold_refusing(session, mod) -> list[str]:
    """The in-hold re-check with `build` acquires refused: returns the commands
    the pass would have compiled (empty = the tree is warm and every prebuild
    was a stamp read, exactly the pass the warm pass promised). A failure the
    pass raised for any other reason (a genuine SPA build error on a tree that
    IS warm) propagates as before."""
    marker_fd, marker = tempfile.mkstemp(prefix="e2e-refused-builds-", suffix=".txt")
    os.close(marker_fd)
    os.environ[mod.REFUSE_BUILD_ENV] = marker
    error: Exception | None = None
    try:
        try:
            _prebuild_web_spa(session)
        except Exception as exc:  # a refused `just web-test` exits non-zero
            error = exc
        _prebuild_binaries(session)  # memoizes its own failures, refusals included
    finally:
        os.environ.pop(mod.REFUSE_BUILD_ENV, None)
        try:
            refused = [line for line in open(marker, encoding="utf-8").read().splitlines() if line.strip()]
        except OSError:
            refused = []
        try:
            os.unlink(marker)
        except OSError:
            pass
    if refused:
        return refused
    if error is not None:
        raise error
    return []


def _forget_prebuilds_for_retry() -> None:
    """Drop what a refused in-hold pass memoized — every FAILED entry (a refusal
    is recorded as a failure by the builder that hit it) and the two SPA flags —
    so the next warm pass compiles it holding no lane. A genuine failure is
    compiled once more there, without the lane, and memoized again."""
    global _WEB_SPA_BUILT, _WASM_PANIC_WITNESS_BUILT
    for key, (kind, _value) in list(_BINARY_BUILD_MEMO.items()):
        if kind == "failed":
            del _BINARY_BUILD_MEMO[key]
    _WEB_SPA_BUILT = False
    _WASM_PANIC_WITNESS_BUILT = False


def _prebuild_web_spa(session):
    """Build the SPA HERE — outside every per-test pytest-timeout budget.

    This placement is a CORRECTNESS requirement, not a speed-up. `static_dir`
    used to be the first thing to run `just web-test`, which charged the whole
    build to whichever web test happened to run first, because pytest.ini's
    `timeout = 900` deliberately covers setup/call/teardown (`timeout_func_only`
    stays False so a wedged fixture can't hold the machine — testing.md § point 9).

    That was a BOUND INVERSION. `just web-test` routes wasm-pack through the
    build-slot tool, whose wait for one of the 2 machine-wide 'build'
    slots is bounded by FAUNA_SLOT_TIMEOUT (default 7200s) — 8x the 900s test
    bound it was nested inside. The wasm-onboarding step was also NOT
    build-if-stale gated at the time, so it took a slot on EVERY run, warm or
    cold (an up-to-date rebuild measured 9m10s: ~7m queued behind other
    concurrent builds on the same machine, then `Finished in 0.34s`) — every
    wasm chunk is gated now, and the linux `web-test` probes all nine with
    `build-if-stale --check` before the mutex+slot chain, so a warm tree takes
    no slot; the bound inversion is what this hoist still prevents on a cold
    one. So whenever parallel
    checkouts held both build slots, the first web test died in the queue with
    the product never touched — and the failure surfaced as the journey's own
    assertion timing out against a still-booting page, which reads EXACTLY like
    a product bug.

    That artifact cost this suite multiple debugging cycles: it is what produced
    the "3 fail / 1 pass on identical code" that was mistaken for a launch-flow
    race and written up as a probe-timeout root cause for the crash-recovery
    factory-reset journey. The e2e slot above is already acquired here for the
    same reason; the build slot simply leaked inside a test.

    Kept bounded: `build-slot.py` names its holders once a minute and fails with
    that diagnosis after its own timeout, so this never hangs silently.
    """
    if "web" not in get_available_apps():
        return
    # Only pay for it when this run actually drives the SPA. The client
    # parametrization lands in the nodeid (`test_x[web]`), and the `web` marker
    # covers the web-only suites that aren't client-parametrized. A run with
    # neither (e.g. `--client linux`) must not build the SPA at all.
    if not any(
        "[web]" in item.nodeid
        or "web" in item.nodeid.partition("[")[2]
        or "web" in {m.name for m in item.iter_markers()}
        for item in session.items
    ):
        return
    # wasm-panic-witness FIRST: it `cp`s its output straight into
    # apps/fauna-web/static/, and `just web-test`'s SvelteKit build copies
    # static/ into build/ exactly once, at build time — a witness chunk that
    # lands in static/ AFTER that copy never reaches build/, so the browser's
    # dynamic `import()` 404s (surfaces as a bridge 500, not a build error).
    # A checkout whose build cache has never built the witness chunks before
    # hits this on every run, deterministically, not as a flake.
    if any("test_wasm_panic_hook" in item.nodeid for item in session.items):
        _ensure_wasm_panic_witness_built()
    _ensure_web_spa_built()


# Fixture name -> (module-global builder name, args). Resolved through
# globals() at call time so tests can monkeypatch the builders. Deliberately
# NOT here: `bench_nest_binary` (its FAUNA_BENCH-gated suites collect-then-skip
# on ordinary runs, and a release nest build is too heavy to spend on an
# overshoot), and `linux_app_path`/`tui_app_path` (skip-only fixtures — they
# never build, and hoisting them would silently change that contract).
# `sync_agent_binary` WAS excluded here too ("bare cargo-win, no slot
# involved") — no longer true since it routes through
# `common.nest.build_sync_service_win`'s gate-then-slot composition.
#: The prebuild jobs that compile a local `fauna-nest`. Skipped whole in a mode
#: whose nest comes from somewhere else (`nest_mode.builds_local_nest`).
_LOCAL_NEST_BUILD_FIXTURES = frozenset({
    "nest_binary", "node_binary", "bluesky_nest_binary", "bridges_nest_binary",
})

_PREBUILD_BY_FIXTURE = (
    ("nest_binary", "_ensure_nest_built", ()),
    # tests/platform/conftest.py's name for the same nest build.
    ("node_binary", "_ensure_nest_built", ()),
    ("bluesky_nest_binary", "_ensure_bluesky_nest_built", ()),
    ("bridges_nest_binary", "_ensure_bridges_nest_built", ()),
    # tests/api/test_activitypub_federation.py's module-local AP build.
    ("ap_binary", "_ensure_ap_nest_built", ()),
    ("mail_bridge_binary", "_ensure_mail_bridge_built", ()),
    ("atproto_bridge_binary", "_ensure_atproto_bridge_built", ()),
    ("atproto_bridge_e2e_binary", "_ensure_atproto_bridge_e2e_built", ()),
    ("seal_helper_binary", "_ensure_seal_helper_built", ()),
    ("sync_agent_binary", "_ensure_sync_agent_built", ()),
    ("macos_app_path", "_ensure_app_built", ("macos",)),
    ("windows_app_path", "_ensure_app_built", ("windows",)),
    ("linux_app_path", "_ensure_app_built", ("linux",)),
    ("tui_app_path", "_ensure_app_built", ("tui",)),
    ("ios_setup", "_ensure_app_built", ("ios",)),
    ("pkg_path", "_ensure_macos_pkg_built", ()),
)


def _apps_for_seat_pair(value):
    """The app names a convention-16 seat pair will launch — `()` if `value`
    isn't a seat pair at all.

    A two-seat pair is parametrized as a tuple of seat MODES
    (`("native", "native")`), never as app names, so `_prebuild_binaries`'
    string scan is blind to it. Resolution goes through
    `sync_seats.native_app_for_platform`, the same single point `_make_seat`
    uses, so the prebuild and the launch can never disagree about which app a
    `native` seat means.
    """
    # Seat modes are strings; a tuple of anything else (a case carrying a set)
    # is not a pair, and `set(value)` below would raise on an unhashable one.
    if not isinstance(value, (tuple, list)) or not value:
        return ()
    if not all(isinstance(mode, str) for mode in value):
        return ()
    from helpers import sync_seats

    if not set(value) <= set(sync_seats.SEAT_MODES):
        return ()
    names = []
    for mode in value:
        if mode == "native":
            native = sync_seats.native_app_for_platform(sys.platform)
            if native:
                names.append(native)
        else:
            names.append(mode)
    return tuple(names)


#: Fixtures that launch a HARD-NAMED app the requesting item's parametrization
#: does not mention — the cross-app sender/receiver pairs. `_prebuild_binaries`
#: reads this so that second app is built at collection time like every other,
#: instead of inside the first requesting test's pytest-timeout budget.
#:
#: Add an entry whenever a fixture calls `create_driver("<app>")` with a literal
#: app name. The cost of forgetting is not a missing build — the fixture still
#: builds it — but a `Timeout (>900.0s)` pointing at `subprocess.wait`, which
#: reads as a product bug and costs a ~90-minute run to discover.
#: ⚠ **LITERAL is the whole membership test**, and the entry that taught it was
#: `caldav_mailbox_less_attendee_app`: it was linux-only when it joined, then
#: grew tui/macos/ios arms and became client-MATCHED (`create_driver(app_name)`
#: after branching on `app.driver.is_tui()` and friends) while its entry stayed
#: `"linux"`. A matched fixture launches the app the item is already
#: parametrized by, so the scan above has it covered and the entry only
#: over-builds — measured 2026-09-02 on the primary Linux dev VM, where a
#: `--nest docker --app tui` run opened with `[prebuild] app[linux]`, a cold
#: `just linux-debug` behind a machine-wide build-slot wait, for an app whose
#: driver that run never creates.
#: Pinned by `test_harness_self_termination.py::
#: test_every_cross_app_hoist_names_an_app_its_fixture_literally_launches`.
_CROSS_APP_FIXTURE_APPS = {
    # A real-engine linux GUI sender driving a web (or other) receiver.
    "real_faunamls_linux_sender": "linux",
    # The desktop seat(s) that build the index a phone under test queries. The
    # fixture launches on demand, through `_alice_extra_seat("macos", …)`; the
    # pin follows the literal one call up.
    "alice_builder_seats": "macos",
    # The user's other computer on a viewer column (web and the mobiles make no
    # index pin and never build): `test_task_delegation.py::_Seats` launches tui.
    "delegation_seats": "tui",
}


def _item_param_apps(item, available: set) -> set:
    """The apps an item's own parametrization launches, among `available`
    (convention-16 seat pairs included) — empty for an item with no app
    parametrization. One scan for the prebuild and the lane choice."""
    callspec = getattr(item, "callspec", None)
    if callspec is None:
        return set()
    apps: set = set()
    for value in callspec.params.values():
        if isinstance(value, str) and value in available:
            apps.add(value)
            continue
        # A convention-16 seat pair names its apps by seat MODE, not by app
        # name, so the plain string scan above cannot see them.
        for name in _apps_for_seat_pair(value):
            if name in available:
                apps.add(name)
    return apps


def _prebuild_app_set(session, requested: set) -> set:
    """The apps `_prebuild_binaries` builds for this run: the items' own
    parametrization (convention-16 seat pairs included) plus the hard-named
    apps of cross-app fixtures in `requested` (the run's real fixture
    closure), minus an installer run's dev windows app. Split out so the e2e
    lane choice (`_run_app_set`) reads the same scan the prebuild does.
    """
    available = set(get_available_apps())
    apps: set = set()
    for item in session.items:
        apps.update(_item_param_apps(item, available))
    # A CROSS-APP fixture launches an app the item's own parametrization never
    # names, so neither scan above can see it — and the app it launches is the one
    # whose `just` build then runs inside the requesting test's 900 s budget, which
    # is the entire failure this function exists to prevent. Observed 2026-08-15:
    # `test_conv_rail_push_wakes_web[web]` died in setup as a bare
    # `Timeout (>900.0s)` on `['just', 'linux-debug']` after 1220 s queued at
    # position 3 of 3 — the linux app was never prebuilt because the item is
    # parametrized `[web]`. The latent twin is
    # `test_fauna_mls_web_receives_from_linux_sender`, same fixture, same shape.
    #
    # Keyed on the fixture NAME (the closure above is already computed), so a
    # fixture that launches a second app declares it here once. Laziness is
    # preserved: the entry only fires for a run whose closure actually contains it.
    #
    # ⚠ Bounded by `sweep_apps()` — this machine's drivable set — NOT by
    # `available`, which is the run's `--app` SELECTION. The distinction is the
    # whole point and cost a second 90-minute run to learn: these fixtures launch
    # a hard-named app *regardless* of `--app`, so under `--app web` (the only
    # selection that reaches `real_faunamls_linux_sender` at all) `available` is
    # exactly `{"web"}` and an `implied in available` guard filters out the one
    # app the hoist exists for. Bounding by the machine instead keeps the guard
    # that matters — never build linux on macOS — without re-introducing the
    # inversion.
    machine_apps = set(sweep_apps())
    for fixture_name, implied in _CROSS_APP_FIXTURE_APPS.items():
        if fixture_name in requested and implied in machine_apps:
            apps.add(implied)
    # The installer full-journey suite drives the MSI-INSTALLED app — never
    # prebuild the dev app it won't launch (mirrors _build_app_config).
    if "windows" in apps and any(
        item.get_closest_marker("installed_product") for item in session.items
    ):
        apps.discard("windows")
    return apps


def _prebuild_binaries(session, *, prewarm: bool = False):
    """Build every binary this run will need HERE — outside every per-test
    pytest-timeout budget. Same correctness requirement as `_prebuild_web_spa`
    above, reached by a more ordinary path: the session fixtures
    (`nest_binary`, `mail_bridge_binary`, `seal_helper_binary`) and the app
    build (`_ensure_app_built`) used to run their `just`/cargo during setup
    of the first test that requested them, and the `just` recipes acquire the
    machine-wide 2-slot `build` pool UNCONDITIONALLY — before cargo or
    `build-if-stale` can conclude there is nothing to do — with the wait
    bounded at 5400 s, nested inside `timeout = 900`. Under fleet contention a
    perfectly good mail/CalDAV/CardDAV test died in setup as a bare
    `Timeout (>900.0s)` pointing at `subprocess.run(["just", ...])`, which
    reads exactly like a product bug (observed twice on 2026-07-29,
    `test_addressbook.py --app tui`; build-system.md § Build/e2e slot locks).
    Pre-building outside pytest never fixed this — it removes the compile,
    not the slot acquisition.

    Laziness is preserved exactly: an item's `fixturenames` is its transitive
    fixture closure, so a run selecting no mail test still never builds the
    bridge, and the app set comes from the items' own parametrization. A
    build failure here is memoized and replayed by the requesting fixture
    (correct attribution, no re-queue) while unrelated tests keep running —
    so failures are printed, not raised, and never abort the whole run.

    The build waits stay loud and bounded (build-slot waiters name their
    holders once a minute, `FAUNA_SLOT_TIMEOUT` — testing.md § point 9). A
    warm tree takes no slot here at all: every builder this hoist reaches gates
    freshness OUTSIDE `{{slot_build}}` (the nest/sync variant stamps, the
    `--stamp` gates of tui-debug / linux-debug / e2e-ffi / mail-bridge-ffi, the
    linux `web-test` chunk precheck), and 14 days of runs captured on the
    primary dev VM (2026-09-26) bear it out: the warm
    app prebuilds took no slot, and the waits that remain precede real
    incremental compiles on trees a rebase just moved. A cold tree's wait is
    no longer charged to any test's clock. Proofs:
    test_harness_self_termination.py § 5.
    """
    # Real fixture requests only — a DIRECTLY-parametrized argname also sits in
    # `fixturenames` and would prebuild the fixture it happens to share a name
    # with (`_is_real_fixture`; that collision on `app` wedged the whole tier_1
    # `test_app_surface_declarations.py` suite behind a cold nest build).
    requested: set = set()
    for item in session.items:
        requested.update(_real_fixture_closure(item))

    apps = _prebuild_app_set(session, requested)

    # The isolated-agent seam reaches `sync_agent_binary` through
    # `_apply_isolated_sync_agent_env`'s `request.getfixturevalue(...)` at LAUNCH
    # time, not through any item's declared fixture closure — so the name-keyed
    # scan above cannot see it and the agent's cold cargo build (plus its
    # machine-wide `build`-slot wait) lands inside the first requesting test's
    # 900 s budget. That is the exact bound inversion this hook exists to
    # prevent, and it is the harness trap that cost the windows trickle-down
    # batch a run: the posture
    # is session-wide, so an UNRELATED test in the same invocation pays the
    # build and dies.
    #
    # ⚠ Since the 2026-09-21 flip this fires for ~every windows run, not only a
    # marker-carrying one, so the hoist is now load-bearing for the WHOLE windows
    # suite rather than one seam — and it must read the posture from the same
    # single home the launch env does, or the two
    # drift and the build lands inside a test's clock again. The `"windows" in
    # apps` guard keeps a run that launches no windows app from building it.
    if "windows" in apps and windows_isolates_its_sync_agent(session.items):
        requested.add("sync_agent_binary")

    # A mode whose nest is not a local binary never executes one, so building it
    # is a cold cargo compile — and a machine-wide `build`-slot wait — spent on
    # an artifact the run discards. `nest_binary` refuses outright in those
    # modes; this is the other half, keeping the build out of collection too.
    #
    # Standalone gets the mirror correction: `nest_instance` resolves
    # `nest_binary` lazily now, so its name is no longer in any item's declared
    # closure, and without this the build would fall back into the first
    # requesting test's setup — inside `timeout = 900`, which is the bound
    # inversion this whole hook exists to prevent. The trigger is the same
    # fixture set `_session_primary_mail_domain` uses to decide "does this
    # session touch the local nest at all", including the plain-`app` tests whose
    # nest is resolved lazily by `_driver_cache`.
    from helpers import nest_mode as nest_mode_mod

    if nest_mode_mod.builds_local_nest(nest_mode_mod.run_mode()):
        if requested.intersection(_LOCAL_NEST_FIXTURE_USERS):
            requested.add("nest_binary")
        # A fixture naming its own build to the seam (`binary=`) resolves it
        # through `getfixturevalue`, invisible to the closure scan — same
        # correction, one table (`nest_surface.DEDICATED_NEST_BINARIES`).
        from helpers import nest_surface as _ns

        for fixture, build in _ns.DEDICATED_NEST_BINARIES.items():
            if fixture in requested:
                requested.add(build)
        if requested.intersection(_MAIL_VENUE_FIXTURE_USERS):
            requested.add("mail_bridge_binary")
            requested.add("seal_helper_binary")
    else:
        requested -= _LOCAL_NEST_BUILD_FIXTURES
    # Any run touching a nest, in any mode, may seed through the `fauna_ffi`
    # builders — lazily, from inside a test body — so the cdylib is loaded here
    # instead (`_ensure_fauna_ffi_loaded`). Observed 2026-09-25: both
    # `test_profile.py` offers tests died as bare `Timeout (>900.0s)` on the
    # in-test `cargo build -p fauna-ffi`.
    load_ffi = bool(requested.intersection(_LOCAL_NEST_FIXTURE_USERS))

    jobs = [
        (fixture_name, builder, args)
        for fixture_name, builder, args in _PREBUILD_BY_FIXTURE
        if fixture_name in requested
    ]
    if load_ffi:
        # Build, then load: the compile half is a freshness-gated recipe the
        # WARM PASS runs holding no lane; the load half is one-shot and stays
        # in the hold (`_NOT_PREWARMED_BUILDERS`), where its own recipe call is
        # then a stamp read. Before 2026-09-29 only the load job existed, so
        # the cdylib queued for `build` inside every hold.
        jobs.append(("fauna-ffi cdylib (build)", "_ensure_fauna_ffi_built", ()))
        jobs.append(("fauna-ffi cdylib", "_ensure_fauna_ffi_loaded", ()))
    jobs.extend(
        (f"app[{name}]", "_ensure_app_built", (name,)) for name in sorted(apps)
    )
    # LAST, and only when both apple apps are selected: the macOS app build
    # (`just mac-debug` → `just apple-ffi-host-test`) deletes the multi-slice
    # FaunaFFI.xcframework the iOS build needs and reassembles it host-only, and
    # `sorted(apps)` puts `ios` before `macos` every time. See
    # `_restore_full_apple_ffi` / `_xcframework_slices`.
    if {"ios", "macos"} <= apps:
        jobs.append(("full FaunaFFI.xcframework restore", "_restore_full_apple_ffi", ()))
    # iOS's simulator is the same bound inversion as a binary build, and a worse
    # one: `_get_ios_setup` creates a throwaway device and boots it, and a device
    # is not usable when `simctl list` first says `Booted` — it is usable when it
    # has FINISHED booting, which was measured at 513 s against 1.1 s on this box
    # (`drivers/ios.py::_ensure_booted`). Left to the first requesting test, that
    # whole wait lands inside its 900 s budget, and before `_ensure_booted`
    # waited for completion it landed there as ~9 minutes of `simctl` calls
    # timing out against a device that was still coming up — a verdict tracking
    # the device's boot rather than the behaviour under test, which is exactly
    # what this hook exists to prevent. Memoized, so the fixtures that call it at
    # setup (`_build_app_config`, `ios_setup`) become cache hits.
    if "ios" in apps:
        jobs.append(("ios simulator (create + finish booting)", "_get_ios_setup", ()))
    for label, builder, args in jobs:
        if prewarm and builder in _NOT_PREWARMED_BUILDERS:
            continue
        print(
            f"[prebuild] {label} (collection time — outside every per-test "
            f"timeout budget{'; warm pass, no e2e slot held' if prewarm else ''})",
            flush=True,
        )
        try:
            globals()[builder](*args)
        except Exception as exc:
            print(
                f"[prebuild] {label} FAILED — tests that request it will "
                f"error with this failure: {exc}",
                flush=True,
            )


def _login_admin_as(app, request, nest, *, spa_url_fixture: str, fixture_name: str) -> None:
    """Log ``app`` in as ``nest``'s admin and land it on the admin shell — the one
    state-protocol admin login every ``*admin_app`` fixture shares; only the nest,
    and for web the SPA proxy that reaches it (``spa_url_fixture``), differ.

    The admin shell's root-stack child exists on every app regardless of the
    `am_i_admin` check, so navigation works even before that check resolves (admin
    *data* still gates on it). Non-bridge drivers (no test agent / set_state) skip
    — there is no UI login path for the admin identity.
    """
    import time
    from drivers.http_bridge import HttpBridgeDriver

    if not isinstance(app.driver, HttpBridgeDriver):
        pytest.skip(f"{fixture_name} requires a bridge-backed driver (web/linux/...)")

    # Named by a relaunch before the session points at `nest` — escrow trust
    # is captured at launch (`_relaunch_trusting_nest`).
    _relaunch_trusting_nest(app.driver, nest)
    admin_secret = nest["admin"]["signing_key"].encode().hex()
    # Resolve the SPA proxy lazily — it transitively requires the built web SPA
    # (`static_dir`), which is unavailable (and skips) on a linux-only run.
    # Mirrors `logged_in_app`.
    node_url = (
        request.getfixturevalue(spa_url_fixture)
        if app.driver.is_web()
        else nest["url"]
    )
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": admin_secret,
            # The macOS/iOS test agent only builds the authenticated FaunaClient
            # (→ the APIClient every admin VM needs) when the session patch carries
            # all of node_url + secret_hex + device_id (FaunaMacApp.applySessionPatch);
            # without device_id the admin shell renders but every data load fails
            # "AdminVM not configured". Mirror logged_in_app, which already sends it.
            "device_id": _E2E_LOGIN_DEVICE_ID,
        },
        "nav": {"stack": [{"view": "admin"}]},
    })
    if app.driver.is_web():
        # The web SPA's WASM auth + admin-check settles asynchronously; the
        # former web-only fixture slept here, so preserve that timing.
        time.sleep(8)
    else:
        # Bridge-backed native apps (linux) rebuild the authenticated window
        # on a `session` patch; wait for the admin shell's default (dashboard)
        # sub-page to confirm the window is up before the test navigates.
        app.driver.wait_for("admin-dashboard-heading", timeout=15.0)


@pytest.fixture
def admin_app(request, app, nest_instance):
    """An ActionLayer logged in as the nest admin, on any bridge-backed driver.

    Generalizes the former web-only fixture: it reuses the parametrized `app`
    (one per available client) and injects the nest admin identity via the state
    protocol (`_login_admin_as`), mirroring `logged_in_app` but with the admin
    signing key and a nav stack pointing at the admin shell.
    """
    _login_admin_as(app, request, nest_instance, spa_url_fixture="spa_url", fixture_name="admin_app")
    yield app


@pytest.fixture
def admin_main_app(request, app, nest_instance):
    """An app logged in as the nest admin but landed on the **primary** view (not
    the admin shell) — so the gated `admin-tab`'s visibility is driven purely by
    the client's admin-status gate (e.g. windows `MainPage.CheckAdminStatusAsync`
    -> `fauna.account.am_i_admin`), NOT by a forced navigation into the shell.

    Why this exists separately from `admin_app`: `admin_app` injects a nav stack
    pointing straight at the admin shell, which on several clients force-reveals
    the admin nav as a side effect of navigating (windows
    `NavigateToAdminSubPage` -> `ShowAdminNavItems`), MASKING a broken gate. A
    real menu-visibility test must NOT navigate into the shell — it must log in as
    admin, stay on the primary view, and assert the gate revealed `admin-tab`.
    See `test_admin_tab_visible_for_admin`.
    """
    from drivers.http_bridge import HttpBridgeDriver

    if not isinstance(app.driver, HttpBridgeDriver):
        pytest.skip("admin_main_app requires a bridge-backed driver (web/linux/...)")
    admin_secret = nest_instance["admin"]["signing_key"].encode().hex()
    node_url = (
        request.getfixturevalue("spa_url")
        if app.driver.is_web()
        else nest_instance["url"]
    )
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": admin_secret,
            "device_id": _E2E_LOGIN_DEVICE_ID,
        },
        # The PRIMARY view (feed), as logged_in_app uses — NOT {"view": "admin"}.
        "nav": {"stack": [{"view": "feed"}]},
    })
    return app


@pytest.fixture(scope="session")
def handled_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest whose **primary mail domain == handle domain ==
    ``MAIL_PRIMARY_DOMAIN``**, so a *handled* actor's canonical
    ``<handle>@<domain>`` is the routable primary mailbox — the precondition for
    any e2e that needs the nest to flag a ``list_account_aliases`` row
    ``is_canonical`` (`mail-aliases.md` § Aliases UX / § Disable).

    Why a separate nest from the shared ``nest_instance``: ``register_handled_actor``
    needs the nest to HAVE that handle domain (the registration signature is over
    ``actor_id || handle || domain`` and the nest recomputes ``domain`` from its
    own resolved handle domain) **and** an open registration posture, which this
    fixture sets after the claim via ``common.auth.open_registration``. The shared
    ``nest_instance`` is started handle-less (so its ``test_user`` /
    ``logged_in_app`` actor has no handle and ``canonical_address_for_actor``
    returns ``None`` — no row is ever canonical). Giving the shared nest a domain
    would change every existing actor's whoami, so the handled flow gets its own
    nest instead.

    It takes **no domain start option**: the ``add_local_domain`` below is what
    registers ``MAIL_PRIMARY_DOMAIN``, and registering the primary IS setting the
    deployment identity (``apply_primary_identity``), so the domain is live
    before this fixture yields. A ``claim_domain`` here would be a second door
    onto the same domain, and the one that lost — ``add_local_domain`` is
    idempotent by domain NAME — would have its ``per_host`` cert mode silently
    discarded (``testing.md`` § Default app and nest mode, ruling (3)).

    Mirrors the shared nest's mail setup: claims ``MAIL_PRIMARY_DOMAIN`` as the
    primary mail domain (the runtime ``local_domains`` row
    ``canonical_address_for_actor`` reads — cf. ``_session_primary_mail_domain``).
    Session-scoped — each test registers its own fresh handled actor over the
    open registration, so they don't share mutable per-actor mail state.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    # Through the mode's provider, not `common.nest.start_nest`: this fixture
    # asks a nest for nothing at all, so it is the zero-option call ruling (1)
    # names as the first able to run in a container.
    #
    # Calling `start_nest` directly was also the one path around
    # `_as_nest_handle`, so until this edit the most widely shared dedicated nest
    # in the suite was the one nest the capability guard never wrapped — and that
    # had already cost a real bug: `_as_nest_handle` is what sets the `peer_url`
    # contract key (the authority ANOTHER nest in the run dials), so a cross-nest
    # test using this fixture as its second nest KeyErrored on
    # `nest_b["peer_url"]`, and a `setdefault` was landed here to patch it. That
    # line is gone with this routing rather than kept beside it: it was a local
    # copy of `_as_nest_handle:1001`, and a fixture that reaches the wrapper
    # cannot want one.
    nest, nest_cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "handled-mail-nest",
    )
    from common.auth import open_registration
    open_registration(nest)
    admin = nest["admin"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as ws:
        ws.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": MAIL_PRIMARY_DOMAIN,
                "mta_sts_cert_mode": "per_host",
            },
        )
    try:
        yield nest
    finally:
        # The provider's own teardown — a container is removed, a process is
        # terminated, and neither is this fixture's business to know.
        nest_cleanup()


@pytest.fixture(scope="function")
def registration_posture_nest(request, nest_mode, tmp_path_factory):
    """A dedicated, **function-scoped** nest for driving the registration posture
    through the admin UI (`admin-users` § Section 2).

    Why not the shared ``nest_instance``: this fixture's whole purpose is to let a
    test *change* the posture, and the posture is deployment-wide nest state. On
    the session nest that would leak into every later test; on the session-scoped
    ``handled_nest`` (opened precisely so each test can register its own handled
    actor) flipping to ``invite_required`` would break every subsequent
    registration. Function scope means the flip dies with the test.

    Opened via ``common.auth.open_registration`` **on purpose, as the negative
    control**: the default posture is ``closed``, so a test that only asserted "a
    stranger is refused" would pass on a nest that had never heard from the UI.
    Opening first makes the refusal attributable to the UI write and nothing else.
    Note this fixture now opens the posture the same way the app does — so the
    control and the behavior under test travel the same wire kind.

    ``claim_domain`` is set because ``register_handled_actor`` signs over
    ``actor_id || handle || domain`` and the nest recomputes ``domain`` from its own
    configured handle domain — a mismatch rejects as ``signature_failed``, which
    would make a refusal assertion pass for the wrong reason.
    """
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reg-posture",
        claim_domain=MAIL_PRIMARY_DOMAIN,
    )
    from common.auth import open_registration
    open_registration(nest)
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def registration_posture_spa_url(static_dir, registration_posture_nest):
    """Function-scoped SPA proxy → the registration-posture nest, so a web browser
    reaches it without CORS. The session ``spa_url`` only proxies ``nest_instance``.
    Mirrors ``handled_spa_url``."""
    url, server = _serve_spa_proxy(static_dir, registration_posture_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def registration_posture_admin_app(request, app, registration_posture_nest):
    """``admin_app``, but pointed at the dedicated ``registration_posture_nest``
    (`_login_admin_as` — only the nest differs). Bridge-backed drivers only.
    """
    _login_admin_as(
        app,
        request,
        registration_posture_nest,
        spa_url_fixture="registration_posture_spa_url",
        fixture_name="registration_posture_admin_app",
    )
    app.registration_posture_nest = registration_posture_nest
    yield app


# `crowded_nest` seeds accounts in batches until its box claimer has left the
# default `fauna.admin.users.list` page, giving up past one full 500-row page.
_CROWDED_NEST_SEED_BATCH = 10
_CROWDED_NEST_SEED_CEILING = 500


@pytest.fixture
def crowded_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest whose box claimer is OLDER than every account on the
    default ``fauna.admin.users.list`` page — the precondition for proving an admin
    actor picker offers every account on the nest, not just the newest page
    (``admin.md`` § 2 → *What identifies a user in an admin picker*).

    Why not the shared ``nest_instance``: pushing its admin off the first page
    would push it off the Users hub's first page too, and every hub test that
    finds the admin's row there (``actions/admin.py::user_row_index``) would fail
    for a reason unrelated to its own. Function scope, because the picker tests
    persist designations (front page, catch-all, role address, a guardian code)
    that a later app parametrization must not inherit.

    Seeds handled accounts until the admin has left the default page — a state
    predicate, not a count: ``created_at`` is second-granular, so accounts created
    in the claim's own second tie with the admin, and a nest that does not break
    ties by insertion order may list a tied admin first.
    """
    import secrets

    from nacl.signing import SigningKey

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "crowded-nest")
    try:
        admin_key = nest["admin"]["signing_key"]
        admin_id = bytes(admin_key.verify_key)
        with WsRpcAdminClient(nest["url"], actor_id=admin_id, signing_key=bytes(admin_key)) as client:

            def admin_on_default_page() -> bool:
                page = client.call("fauna.admin.users.list", {})
                return any(bytes(u["actor_id"]) == admin_id for u in page.get("users", []))

            seeded = 0
            while admin_on_default_page():
                assert seeded < _CROWDED_NEST_SEED_CEILING, (
                    f"seeded {seeded} newer accounts and the box claimer is still on "
                    "the default fauna.admin.users.list page"
                )
                for _ in range(_CROWDED_NEST_SEED_BATCH):
                    handle = f"crowd{secrets.token_hex(4)}"
                    client.call(
                        "fauna.admin.users.create",
                        {
                            "actor_id": bytes(SigningKey.generate().verify_key),
                            "tier": "free",
                            "label": handle,
                            "handle": handle,
                        },
                    )
                    seeded += 1
        yield nest
    finally:
        cleanup()


@pytest.fixture
def crowded_spa_url(static_dir, crowded_nest):
    """Function-scoped SPA proxy → ``crowded_nest``, so a web browser reaches it
    without CORS (the session ``spa_url`` only proxies ``nest_instance``). Mirrors
    ``registration_posture_spa_url``."""
    url, server = _serve_spa_proxy(static_dir, crowded_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def crowded_admin_app(request, app, crowded_nest):
    """``admin_app``, but pointed at ``crowded_nest`` (`_login_admin_as` — only the
    nest differs). Bridge-backed drivers only."""
    _login_admin_as(
        app,
        request,
        crowded_nest,
        spa_url_fixture="crowded_spa_url",
        fixture_name="crowded_admin_app",
    )
    yield app


@pytest.fixture
def handled_logged_in_app(request, app, handled_nest):
    """Like ``logged_in_app``, but the actor has a **claimed handle** on the
    ``handled_nest`` (primary mail domain == handle domain), so enabling mail
    auto-mints the canonical ``<handle>@<domain>`` exact alias
    (``ensure_canonical_handle_alias`` via ``provision_recipient_mls_pubkey``) and
    the nest flags its alias row ``is_canonical = true``. The handle-dependent
    counterpart to ``logged_in_app`` (whose actor is handle-less); reusable by any
    future linux e2e that needs the client to see a canonical mail row.

    Exposes the handled actor on the app as ``app.handled_actor`` (dict shaped
    like ``register_handled_actor``: ``signing_key`` / ``actor_id_hex`` /
    ``actor_id_bytes`` / ``token`` / ``handle``) and the nest as
    ``app.handled_nest`` (for WS-RPC ground-truth assertions). Bridge-backed
    drivers only (state-protocol login) — others skip, mirroring ``admin_app``.
    """
    import secrets
    from drivers.http_bridge import HttpBridgeDriver

    if not isinstance(app.driver, HttpBridgeDriver):
        pytest.skip("handled_logged_in_app requires a bridge-backed driver (linux/web/...)")

    # Named by a relaunch before the session points at `handled_nest` —
    # escrow trust is captured at launch (`_relaunch_trusting_nest`).
    _relaunch_trusting_nest(app.driver, handled_nest)

    from common.auth import register_handled_actor

    handle = "alice" + secrets.token_hex(3)
    actor = register_handled_actor(
        handled_nest["port"], handle=handle, domain=MAIL_PRIMARY_DOMAIN,
    )

    secret_hex = actor["signing_key"].encode().hex()
    # Web: the browser must reach the *handled* nest, not the session
    # ``nest_instance`` that ``spa_url`` proxies — so use a dedicated proxy bound
    # to ``handled_nest`` (handled_spa_url). Native drivers point straight at the
    # handled nest URL.
    node_url = (
        request.getfixturevalue("handled_spa_url")
        if app.driver.is_web()
        else handled_nest["url"]
    )
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": secret_hex,
            "handle": handle,
            "actor_id": actor["actor_id_hex"],
            "device_id": _E2E_LOGIN_DEVICE_ID,
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    app.handled_actor = actor
    app.handled_nest = handled_nest
    return app


# ---------------------------------------------------------------------------
# --client filter + test ordering
# ---------------------------------------------------------------------------
# When --client <name> is passed, deselect tests that don't exercise that
# client. This replaces ad-hoc --ignore flag lists per client.
#
# A test runs under --client X iff:
#   - It is genuinely parametrized by a REAL app-selecting fixture (`app`,
#     `persistent_app`, or any test-local fixture indirectly parametrized
#     with app names) whose value for this item is X — never by pattern-
#     matching the parametrize id string, which can coincidentally contain
#     an app-name token for a parametrization that has nothing to do with
#     app selection (`_parametrized_clients` below —
#     a merge-gate name, an OS-`platform` string, and `driver_kind` DATA
#     threaded through the one real `app` under test were all measured
#     false positives); OR
#   - Its name has no client parametrization AND its file is NOT in the
#     client-independent list below.
#
# To include client-independent tests (api contract, driver unit tests,
# scenarios, platform-specific diag), omit --client or use --include-independent.
#
# NOTE: web-SPA-driven tests under these dirs (the Playwright `platform/bridge`
# + `platform/docker` suites) are marked `@pytest.mark.web` — the platform-marker
# deselection below is authoritative and runs *before* the independent/include
# check, so they are dropped under a non-web `--client` EVEN with
# --include-independent (they only run under `--client web` or no `--client`).
# Don't rely on --include-independent to pull web UI tests into a non-web run.
#
# Additionally, honesty/destructive tests are pushed to the end because
# they can crash fragile bridges (XCUITest, AT-SPI, FlaUI).

_KNOWN_APPS = {"web", "ios", "macos", "android", "windows", "linux", "tui"}

# Subdirectories that are client-independent — always deselected under --client.
_CLIENT_INDEPENDENT_DIRS = {"api", "platform", "scenarios", "web"}

# Top-level test files that are client-independent.
_CLIENT_INDEPENDENT_FILES = {
    "test_agent_standalone.py",
    "test_api_helpers.py",
    "test_app_surface_declarations.py",  # tier_1 proofs for the unbuilt/absent/environment skip taxonomy — no driver/app
    "test_bridge_death_classification.py",  # tier_1 unit test of the timeout-vs-death classification — fake in-test bridge, no driver/app
    "test_driver_isolation.py",
    "test_driver_relaunch_pin.py",  # tier_1 unit test of the bridge relaunch-pin lifecycle — no driver/app
    "test_driver_state_api.py",
    "test_enrollment_named_row_readback.py",  # tier_1 pins for the named-row barrier's gates + per-app read-back — no driver/app
    "test_fake_cloud_capacity.py",  # tier_1 unit test of the httpserver fixture's threading — no driver/app
    "test_fauna_ffi.py",
    "test_fauna_ffi_finder.py",  # tier_1 pins of the win cdylib finder against a fake checkout — no driver/app
    "test_harness_self_termination.py",  # harness-of-the-harness: wedge/reap/timeout drills, no app
    "test_imap_fetch_latency_bench.py",  # tier_3 Phase-3 perf gate — nest+MDA only, no client driver
    "test_tui_driver_env.py",  # tier_1 unit test of drivers/tui env logic — no driver/app
    "test_tui_pty_backend.py",  # tier_1 unit test of the pty backend seam — trivial helper, no driver/app
    "test_linux_bridge_env.py",  # tier_1 unit test of drivers/linux env logic — no driver/app
    "test_macos_agent_standalone.py",
    "test_mail_bridge_corun.py",
    "test_mail_bridge_filter_delivery.py",
    "test_mail_bridge_mda.py",
    "test_mail_bridge_mta.py",
    "test_mail_inbound_to_imap.py",
    "test_provisioning.py",
    "test_register_response.py",
    "test_scope_parser.py",
    "test_sp_linux_smoke.py",
    "test_sp_linux_ws_rpc_echo.py",
    "test_sp_linux_ws_rpc_push.py",
    "test_static_serving.py",
    "test_windows_state_diag.py",
    # A full bare `tests/` sweep on Windows picked these up regardless of --app
    # (none carry a client marker/parametrization), producing ~14 spurious
    # FAILEDs unrelated to any windows/scroll change.
    "test_cargo_target_zfs_wrapper.py",  # tier_1, linux/ZFS-specific — no driver/app
    "test_gofmt_gate.py",  # tier_1 in-process gofmt-gate analysis — no driver/app
    "test_go_signed_message_parity.py",  # tier_1 in-process go signed-message-parity analysis — no driver/app
    "test_go_toolchain_pin_sync.py",  # tier_1 in-process go-toolchain-pin analysis — no driver/app
    "test_gate_check_kick_lock.py",  # tier_1 proof of the session-start hook's freshen-lock handoff — no driver/app
    "test_nest_mode_axis.py",  # tier_1 nest-mode seam/flag pins (docker-mode cases need docker) — no driver/app
    "test_root_build_guard.py",  # tier_1 proofs for the sudo-run/root-build guard — no driver/app
    "test_run_ceiling.py",  # tier_1 proofs for the --max-run-secs run ceiling — no driver/app
    "test_rust_toolchain_action_block_parity.py",  # tier_1 YAML analysis of the rust-toolchain workflow block — no driver/app
    "test_shared_crate_seam_gating.py",  # convention 15 tier_1 scan of `_for_test` seams — no driver/app
    "test_workflow_permissions_declared.py",  # tier_1 YAML analysis of workflow permissions blocks — no driver/app
    "test_replay_public_ci_covers_workflow.py",  # tier_1 proof the local CI replay covers the shipped workflow — no driver/app
    "test_throughput_probe.py",  # tier_1 in-process probe of the cross-VM throughput script — no driver/app
    "test_frame_invariants.py",  # tier_1/tier1 post-frame invariant catalogue module — no driver/app
    "test_bundle_scanner_probe_verdicts.py",  # tier_1 verdict-logic proof for the release bundle-scanner probe — no driver/app
    "test_android_cleartext_debug_only.py",  # convention 15 tier_1 source-set scan of the android cleartext policy — no driver/app
    "test_android_device_serial.py",  # tier_1 pins for the run-level android device axis — no driver/app
    "test_android_driver_adb.py",  # tier_1 adb command-line pins against a fake adb — no driver/app/device
    "test_android_venue.py",  # tier_1 pins for the android venue's fixed ports, leases and tunnel spec — no driver/app
    "test_windows_driver_infobar_text.py",  # tier_1 pins of the windows driver's InfoBar-text strip against a canned read — no driver/app
}


def _parametrized_clients(item) -> set[str]:
    """Return the set of KNOWN_APPS this item is genuinely parametrized over.

    Keys on `_is_real_fixture` per `callspec.params` entry, never on the
    parametrize id string. A directly-parametrized argname on the test
    itself (`@pytest.mark.parametrize("platform", ["linux", ...])`,
    `@pytest.mark.parametrize("gate", [..., "android-unit-test-compile-check"])`)
    is serviced by pytest's own synthesized pseudo-fixture and excluded even
    when its value collides with a known app name — only a genuinely REAL
    fixture (`app`, `persistent_app`, or any test-local fixture indirectly
    parametrized with app names, whatever its own name) means this item's
    identity depends on which app is running here.
    """
    callspec = getattr(item, "callspec", None)
    if callspec is None:
        return set()
    return {
        v for k, v in callspec.params.items()
        if isinstance(v, str) and v in _KNOWN_APPS and _is_real_fixture(item, k)
    }


def _has_empty_parameter_set(item) -> bool:
    """Is `item` pytest's placeholder for an empty parameter set (`[NOTSET]`)?

    Pytest fills every argname of such an item with its `NotSetType` sentinel —
    keyed on the value's type name rather than an import of pytest's private
    module, and pinned against the real sentinel in
    `tests/test_app_default_and_sweep.py`."""
    callspec = getattr(item, "callspec", None)
    if callspec is None:
        return False
    return any(type(v).__name__ == "NotSetType" for v in callspec.params.values())


#: Parametrized fixtures whose value is a REAL second app instance the fixture
#: itself launches via `create_driver(request.param)` — `folder_share_owner_app`/
#: `folder_share_recipient_app`/`folder_share_stranger_app` (`@pytest.fixture(params=[...])`, conftest.py),
#: which stand up a genuinely SECOND GUI process alongside the primary `app`
#: fixture's. Deliberately NOT the same question `_parametrized_clients` above
#: answers (any real app fixture's value in `selected` ⇒ keep): a value from
#: one of THESE fixtures means a real driver for it must exist on THIS machine,
#: so ALL of them must be in `selected`, not just one. Conflating the two would
#: break e.g. `test_handle_entry_outcomes.py`'s `driver_kind` parametrization,
#: whose values (`"macos"`, `"windows"`, …) are rendered-outcome DATA fed
#: through the one real `app` under test, never a second launched app —
#: `_parametrized_clients` already excludes it (`driver_kind` is a direct
#: param on the test, not a real fixture), so an item like `[tui-macos]`
#: there is correctly kept on a Linux dev machine on `app`'s "tui" alone.
_REAL_SECOND_APP_FIXTURES = {
    "folder_share_owner_app",
    "folder_share_recipient_app",
    "folder_share_stranger_app",
    # The Media read witnesses' member seat (tui external-open / web download).
    "media_member",
    # The shared-and-served WebDAV journey's member seat
    # (`test_webdav_shared_set.py`).
    "served_share_member",
}


def _is_client_independent(item) -> bool:
    """True if the test's file is in the client-independent list."""
    path = Path(str(item.path))
    name = path.name
    # Walk up to find 'tests' directory; check if parent dir is in independent dirs
    for parent in path.parents:
        if parent.name == "tests":
            break
        if parent.name in _CLIENT_INDEPENDENT_DIRS:
            return True
    if name in _CLIENT_INDEPENDENT_FILES:
        return True
    return False


# tests/platform/<dir> holds host-OS-specific suites (installer tests, native
# IPC probes, …); the dir name names the OS it requires. Some of them import
# OS-only modules at module scope (`pwd`, `os.getuid`) — collecting them on the
# wrong host OS raises ImportError/AttributeError before their own
# `sys.platform` skipif ever gets a chance to run.
_PLATFORM_OS_DIRS = {
    "linux": "linux",
    "macos": "darwin",
    "windows": "win32",
}


def _wrong_os_platform_subtree(path: Path) -> bool:
    """True if path sits under tests/platform/<os> for an OS other than this one."""
    parts = path.parts
    if "platform" not in parts:
        return False
    subdir_idx = parts.index("platform") + 1
    if subdir_idx >= len(parts):
        return False
    required = _PLATFORM_OS_DIRS.get(parts[subdir_idx])
    return required is not None and not sys.platform.startswith(required)


def pytest_ignore_collect(collection_path, config):
    """Skip client-independent test files early, before import.

    Some client-independent tests import modules that aren't available on every
    platform (e.g. `playwright`, `pwd`, `os.getuid`). Without this hook those
    files would raise ImportError during collection and fail the whole run,
    even though pytest_collection_modifyitems would have deselected them.
    """
    path = Path(str(collection_path))
    # Real-ambient-session tests (tests/real_session/) mutate the box's real
    # desktop session and are NEVER collected without the explicit opt-in flag —
    # a harder gate than an in-test skip, so no default sweep, tier filter, or
    # recipe can even import them. Checked before the is_dir early-return so the
    # whole directory is pruned in one step.
    if "real_session" in path.parts and not config.getoption("--real-session"):
        return True
    # Client-artifact tests (tests/artifact/) build and drive a real SHIPPED
    # artifact — packaging work per module, and a subject the default inner loop
    # deliberately does not use. Same hard gate as real_session/ and for the same
    # reason: an in-test skip would still import the module and let a tier filter
    # or a `just` sweep pull it in, whereas pruning the directory means only an
    # explicit opt-in can reach it. `--macos-artifact` implies `--client-artifact`
    # because it is the apple-flavoured superset — it ALSO swaps what `--app macos`
    # launches — and predates the category being app-agnostic; that directory's own
    # conftest then prunes the modules whose OS is not this box's.
    if "artifact" in path.parts and not (
        config.getoption("--client-artifact") or config.getoption("--macos-artifact")
    ):
        return True
    # Convention 17 layer (c)'s walk sweep (tests/walk/) — hundreds of round
    # trips over the real binary, and by the convention's own wording a
    # SCHEDULED sweep, never inner-loop. Pruned rather than skipped for the same
    # reason as the two directories above: a skip still imports the module and
    # leaves a tier filter or a `just` recipe able to pull it in.
    if "walk" in path.parts and not config.getoption("--walk-sweep"):
        return True
    if path.is_dir():
        return None
    if _wrong_os_platform_subtree(path):
        return True

    client_opt = config.getoption("--app") or config.getoption("--client")
    include_independent = config.getoption("--include-independent")
    if not client_opt or include_independent:
        return None
    name = path.name
    for parent in path.parents:
        if parent.name == "tests":
            break
        if parent.name in _CLIENT_INDEPENDENT_DIRS:
            return True
    if name in _CLIENT_INDEPENDENT_FILES:
        return True
    return None


_REPO_ROOT = Path(__file__).resolve().parents[2]


def _item_features(item) -> list:
    """The slugs an item's `@pytest.mark.feature(...)` markers name, in order."""
    slugs = []
    for marker in item.iter_markers("feature"):
        for arg in marker.args:
            if isinstance(arg, str) and arg not in slugs:
                slugs.append(arg)
    return slugs


def _repo_node_id(config, nodeid: str) -> str:
    """A rootdir-relative node id as the repo spells it: `tests/e2e-unified/…::test_x`.

    The catalog's contracts cite repo-relative paths — that is what makes them live
    links in the published tree — while pytest's own ids are relative to the rootdir
    (`tests/e2e-unified/`, where pytest.ini sits). Normalising here means the ledger
    keys and the contract ids are the same strings, and the parametrization is
    dropped so every `[tui-docker]` / `[web-standalone]` variant of one test collapses
    to the one line the contract names.
    """
    path, _, rest = nodeid.partition("::")
    rest = re.sub(r"\[.*\]$", "", rest)
    try:
        prefix = config.rootpath.relative_to(_REPO_ROOT).as_posix()
    except (ValueError, AttributeError):
        prefix = ""
    full = f"{prefix}/{path}" if prefix and prefix != "." else path
    return f"{full}::{rest}" if rest else full


def _apply_feature_axis(config, items) -> None:
    """Validate every `feature` marker against the pages, then apply `--feature`.

    An unknown slug is a **collection error**, exactly like the strict `tier_N` check
    (feature-catalog.md § The marker) and for the same reason: a typo must not
    silently mint a feature nobody has a page for, and the place to catch it is where
    it was written rather than at merge.
    """
    from helpers import feature_ledger

    known = feature_ledger.known_slugs()
    unknown = []
    for item in items:
        for slug in _item_features(item):
            if known is not None and slug not in known:
                unknown.append(f"{item.nodeid}: feature({slug!r})")
    if unknown:
        raise pytest.UsageError(
            "@pytest.mark.feature names slug(s) with no page in docs/features/:\n  "
            + "\n  ".join(unknown[:20])
            + (f"\n  ... ({len(unknown)} total)" if len(unknown) > 20 else "")
            + "\n\nFix: create docs/features/<slug>.md (feature-catalog.md § The page), "
              "or correct the slug. `just features-lint` checks the same rule from the "
              "other side."
        )

    wanted = list(config.getoption("feature") or [])
    if wanted:
        unmatched = [s for s in wanted if known is not None and s not in known]
        if unmatched:
            raise pytest.UsageError(
                "--feature names slug(s) with no page in docs/features/: "
                + ", ".join(sorted(unmatched))
            )
        kept, deselected = [], []
        for item in items:
            (kept if set(_item_features(item)) & set(wanted) else deselected).append(item)
        if deselected:
            config.hook.pytest_deselected(items=deselected)
            items[:] = kept

    # What the run SELECTED — the whole-set check that decides whether a cell may be
    # re-stamped at all (§ Cell semantics). Recorded for every run, ledger or not:
    # it costs a set insert and the flag is read once, at write time.
    tagged = {}
    node_ids = {}
    _selected_repo_ids.clear()
    for item in items:
        repo_id = _repo_node_id(config, item.nodeid)
        # Every surviving item, tagged or not: the gates audit subtracts this
        # set from a page's contract to tell a mode exclusion apart from an
        # app-axis deselection or a stale citation, and an untagged item is
        # exactly as capable of being cited as a tagged one.
        _selected_repo_ids.add(repo_id)
        slugs = _item_features(item)
        if not slugs:
            continue
        tagged[item.nodeid] = slugs
        node_ids[item.nodeid] = repo_id
        for slug in slugs:
            feature_ledger.note_collected(slug, repo_id)

    if not tagged or not feature_ledger.enabled(config):
        return

    # Resolve the run's context ONCE — a git call and at most one `docker inspect` —
    # so the per-report hook stays a dict write. Collection runs after
    # `pytest_configure`, so the nest mode is already settled here.
    from helpers import nest_mode as nest_mode_mod

    mode = nest_mode_mod.run_mode()
    candidate = bool(config.getoption("release_candidate"))
    if candidate:
        refusal = feature_ledger.release_candidate_refusal(mode)
        if refusal:
            raise pytest.UsageError(
                "--release-candidate refused: " + refusal
                + "\n\nA bare-version stamp claims the run went against the released "
                  "artifact at the commit carrying that version, so the plugin "
                  "re-verifies the preconditions itself rather than trusting the "
                  "recipe (feature-catalog.md § Release-candidate run)."
            )
    version = feature_ledger.workspace_version()
    commit = feature_ledger.head_commit()
    # The tree, asked beside the commit: a commit names the code only when nothing
    # uncommitted sits on top of it, and the default inner loop (edit, run, commit
    # while it builds) is exactly the tree where something does. The run never
    # refuses over it — it MARKS every record and cell (feature-catalog.md § The
    # ledger, *Dirty trees*); a candidate run was already refused above if dirty.
    dirt = feature_ledger.tree_dirt()
    dirty = feature_ledger.is_dirty(dirt)
    # And main's place beneath the commit, asked from the same HEAD: the sha that
    # still resolves after the pre-merge rebase has replaced `commit`
    # (feature-catalog.md § The ledger, *Rebased commits*).
    base = feature_ledger.main_base(commit)
    _feature_run.clear()
    _feature_run.update({
        "features": tagged,
        "node_ids": node_ids,
        "version": version,
        "commit": commit,
        "base": base,
        "nest_mode": str(mode),
        "image_digest": feature_ledger.image_digest_for(mode),
        # The provider's own resolved image — `mode.argument`
        # for a non-docker mode too, since `resolved_image_ref` is `None` there and
        # falls through to it; identical to the old value for every candidate
        # run, which always names an explicit `--nest docker:<ref>` argument.
        "image": feature_ledger.resolved_image_ref(mode) or getattr(mode, "argument", None),
        "candidate": candidate,
        "dirt": dirt,
        "dirty": dirty,
        "stamp": feature_ledger.stamp(version, commit, str(mode),
                                      release_candidate=candidate, dirty=dirty),
    })


_TIER_MARKERS = {"tier_1", "tier_2", "tier_3", "tier_4"}


def _parse_tier_opt(tier_opt: str) -> set[str]:
    """Parse --tier value into a set of marker names. Accepts '1', '2,3', 'tier_1', etc."""
    out = set()
    for tok in tier_opt.split(","):
        tok = tok.strip()
        if not tok:
            continue
        if tok.startswith("tier_"):
            name = tok
        else:
            name = f"tier_{tok}"
        if name not in _TIER_MARKERS:
            raise pytest.UsageError(
                f"--tier expects 1, 2, 3, or 4 (got {tok!r}). See tests/e2e-unified/README.md § Test tiers."
            )
        out.add(name)
    return out


def pytest_collection_modifyitems(config, items):
    """Apply --client and --tier filters, then sort honesty/destructive tests to the end."""
    client_opt = config.getoption("--app") or config.getoption("--client")
    include_independent = config.getoption("--include-independent")
    tier_opt = config.getoption("--tier")

    # `live_box` runs against the real shared box: a single test can legitimately
    # exceed the harness-wide 900s default (repeated ~15-min factory resets, real
    # DNS/ACME waits), and the lock wait for a sibling's slot counts against the
    # same setup phase. Bounded is still the law — bump, never unbound. An
    # explicit @pytest.mark.timeout on the test wins.
    for item in items:
        if (item.get_closest_marker("live_box") is not None
                and item.get_closest_marker("timeout") is None):
            item.add_marker(pytest.mark.timeout(3600))

    # The app axis ALWAYS applies. An absent `--app` means "this machine's
    # default app set" — `get_available_apps()`, i.e. `[tui]` since the
    # 2026-08-01 rust-first flip — never "no app filtering at all". Keying the
    # filter on the *flag* rather than on the effective set is what let a bare
    # run collect every platform-marked test on every machine, which the flip
    # would otherwise have left in place: `get_available_apps()` governed only
    # the parametrization, so a `@pytest.mark.ios` test still collected on a
    # machine with no iOS toolchain.
    selected = set(_resolve_app_tokens(client_opt)) if client_opt else set(get_available_apps())
    # Client-independent suites (tests/api/, ws-rpc, …) stay in the *default*
    # run and are dropped only by an explicit `--app`, which declares an
    # app-scoped run. They are pure nest/Rust tests with no driver — exactly
    # what the rust-first inner loop wants most — and keying their removal on
    # the flag also keeps `pytest tests/e2e-unified/tests/api/` collecting.
    keep_independent = include_independent or not client_opt
    if selected:
        kept = []
        deselected = []
        for item in items:
            # Platform-marked tests (e.g. `@pytest.mark.ios`, `@pytest.mark.web`)
            # only exercise the marked platform(s); deselect when none of them is
            # in --client. This catches single-platform tests that still acquire a
            # [linux]/[web] parametrization via the `app` fixture — without it they
            # are *kept* and then `pytest.skip` in-body under the wrong client,
            # inflating the skip count and hiding genuinely-missing coverage. The
            # marker is authoritative over the parametrization (an iOS-only test
            # parametrized [linux] is still deselected). Registered client markers
            # live in pytest.ini.
            marker_platforms = {m.name for m in item.iter_markers()} & _KNOWN_APPS
            if marker_platforms and not (marker_platforms & selected):
                deselected.append(item)
                continue
            # A test restricted to a client set this run did not select —
            # `_clients(*AUTOSTART_APPS)` on a macos run, a module's
            # `@pytest.fixture(params=_supported())` filtered to nothing — is
            # parametrized over an EMPTY set, and pytest answers that with one
            # `[NOTSET]` placeholder item that skips ("got empty parameter
            # set"). It is not a test of anything here: it records nothing to
            # the feature ledger (an undeclared skip), yet it inflated the skip
            # count and sat in every `--feature` selection as if it were a
            # witness. The run's app axis answers "none of its clients is
            # selected" by deselecting, so this does too — the "Empty (→ no
            # items)" the filter helpers promise. The catalog reads the same
            # restriction by parse (`features_scan.client_param_sets`).
            if _has_empty_parameter_set(item):
                deselected.append(item)
                continue
            # A genuine two-real-app test (both `folder_share_owner_app` and
            # `folder_share_recipient_app` parametrized together, item name
            # e.g. `[tui-macos]`) needs BOTH seats drivable HERE — an ANY-of
            # match (what the `params` check below deliberately does, for a
            # different and more common reason — see `_REAL_SECOND_APP_FIXTURES`)
            # is wrong for this shape: it lets a machine-impossible second seat
            # through collection. Measured 2026-08-17: `--app sweep` on a Linux
            # dev machine selected `test_macos_writer_member_decrypts_owner_upload[tui-macos]`
            # and burned two full failed cross-compile builds (`E0463: can't
            # find crate for std`, `aarch64-apple-darwin` absent) before the
            # in-body skip ever ran — the deselection hook reached the first
            # seat (tui, selected) and never checked the second (macos, not).
            callspec = getattr(item, "callspec", None)
            if callspec is not None:
                real_second_apps = {
                    v for k, v in callspec.params.items()
                    if k in _REAL_SECOND_APP_FIXTURES
                    and isinstance(v, str) and v in _KNOWN_APPS
                }
                if real_second_apps and not real_second_apps <= selected:
                    deselected.append(item)
                    continue
                # The FIRST seat of such a test (the run's cached `app`) is
                # owed its own mark too: the second seat's arm mark joins the
                # item's markers, so the any-of check below would let an `ios`
                # owner through on a tui/macOS-marked module merely because its
                # member arm is `tui` (`test_webdav_shared_set.py[ios-tui]`).
                first_seat_apps = {
                    v for k, v in callspec.params.items()
                    if k not in _REAL_SECOND_APP_FIXTURES
                    and isinstance(v, str) and v in _KNOWN_APPS
                    and _is_real_fixture(item, k)
                }
                if (real_second_apps and marker_platforms
                        and not first_seat_apps <= marker_platforms):
                    deselected.append(item)
                    continue
            params =_parametrized_clients(item)
            if params:
                # Parametrized by client: keep only matching client(s). The param
                # client must be in --client AND — when the test declares client
                # markers — among them. The marker is authoritative over the
                # parametrization (same principle as the marker deselection
                # above): without the `& marker_platforms` clause a `[web]` item
                # survives merely because the test shares *another* mark (e.g. a
                # module-level `linux`) with a multi-app --client, so a
                # lead-client-only test (linux-marked, not web-marked) would run
                # on a `web` driver that has not lifted the feature yet. With no
                # client markers the parametrization alone governs (supports all).
                if (params & selected) and (
                    not marker_platforms or (params & marker_platforms)
                ):
                    kept.append(item)
                else:
                    deselected.append(item)
                continue
            # Not parametrized by client
            if _is_client_independent(item) and not keep_independent:
                deselected.append(item)
            else:
                kept.append(item)
        if deselected:
            config.hook.pytest_deselected(items=deselected)
            items[:] = kept

    # ── The nest axis: classified exclusion, then tier elevation ───────────
    # Both are no-ops on the default path — standalone excludes nothing and
    # elevates nothing — so the rust-first inner loop pays literally nothing for
    # this stage, which is the axis's standing requirement (`testing.md:44`).
    #
    # Runs BEFORE the `--tier` filter on purpose: elevation decides what
    # `--tier 4` means in this run, so it has to happen while the filter can
    # still see it.
    from helpers import nest_mode as nest_mode_mod, nest_surface

    _mode = nest_mode_mod.run_mode()
    if not _mode.is_standalone:
        kept = []
        deselected = []
        for item in items:
            verdict = nest_surface.classify(item, _mode)
            if verdict is None:
                kept.append(item)
                continue
            klass, reason = verdict.klass, verdict.reason
            # The join keys, recorded HERE because this is the last moment they
            # exist: `_apply_feature_axis` tags slugs and normalises node ids
            # further down this same hook, and a deselected item never reaches
            # it. Without them the tally can say "9 tests, class (5)" but not
            # which feature pages that blanks (feature-catalog.md § Cell
            # semantics).
            nest_surface.record_gate(
                _mode.name, item.nodeid, klass, reason,
                repo_id=_repo_node_id(config, item.nodeid),
                features=_item_features(item),
                rule=verdict.rule,
                # Every rule that applies, not just the deciding one: the
                # ratified order puts a MIXED class ahead of a FACT one, so the
                # first match alone overstates how much of the catalog's
                # blankness is closable. Paid only for tests already excluded.
                all_rules=nest_surface.all_rules(item, _mode),
                # The fixture is the unit a fix acts on, so the closure is what
                # turns a list of blanked pages into a work item: 69 witnesses
                # across 22 pages grouped onto a handful of fixtures.
                closure=_real_fixture_closure(item),
            )
            deselected.append(item)
        if deselected:
            config.hook.pytest_deselected(items=deselected)
            items[:] = kept

        # The authored `tier_N` marker is the FLOOR and stays exactly as
        # written — per-file enforcement and `tag-test-tiers.py` are unchanged.
        # What changes is what the test *is* this run: a tier_3 test whose nest
        # is the real image under s6 is exercising packaging and supervision,
        # i.e. it is running at tier_4 whatever its file says. Stamping that
        # makes `--tier 4` truthful per run instead of per file.
        for item in items:
            markers = {m.name for m in item.iter_markers()}
            if "tier_3" in markers and "tier_4" not in markers:
                item.add_marker(pytest.mark.tier_4)

    if tier_opt:
        selected_tiers = _parse_tier_opt(tier_opt)
        kept = []
        deselected = []
        for item in items:
            markers = {m.name for m in item.iter_markers()}
            tier_markers = markers & _TIER_MARKERS
            if tier_markers & selected_tiers:
                kept.append(item)
            else:
                deselected.append(item)
        if deselected:
            config.hook.pytest_deselected(items=deselected)
            items[:] = kept

    _apply_feature_axis(config, items)

    missing = []
    for item in items:
        markers = {m.name for m in item.iter_markers()}
        if not (markers & _TIER_MARKERS):
            missing.append(item.nodeid)
    if missing:
        raise pytest.UsageError(
            "tests missing tier_N marker (tier_1 | tier_2 | tier_3 | tier_4 — see tests/e2e-unified/README.md § Test tiers):\n  "
            + "\n  ".join(missing[:20])
            + (f"\n  ... ({len(missing)} total)" if len(missing) > 20 else "")
            + "\n\nFix: run `scripts/tag-test-tiers.py` (idempotent — auto-classifies new files), "
            + "or add `pytestmark = pytest.mark.tier_N` near the top of each file."
        )

    # Order: honesty/destructive last
    normal = []
    late = []
    for item in items:
        markers = {m.name for m in item.iter_markers()}
        if markers & {"honesty", "destructive"}:
            late.append(item)
        else:
            normal.append(item)
    items[:] = normal + late
