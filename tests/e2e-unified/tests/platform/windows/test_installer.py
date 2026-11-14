"""E2E tests: Windows MSI installer — independent feature installation."""

import glob
import json
import os
import shutil
import subprocess
import sys
import time

import pytest

from helpers.build_step import run_build_step
from helpers.helper_bins import helper_bin
from helpers.windows_installer import (
    is_admin,
    max_mtime,
    msi_is_cached,
    msi_notices_faults,
    msi_paths,
    release_ffi_dll,
    require_release_test_flavor,
    source_mtime,
)

# The publish and the WiX link below are this suite's subject and stay in
# `msi_path`, charged to the first test that asks (e2e-conventions.md point 9: a
# subject build stays in its test, on a 3600 s clock). Everything they consume is
# a collection-time prebuild (`conftest.py::_prebuild_jobs`).
pytestmark = [pytest.mark.skipif(sys.platform != "win32", reason="Windows-only"), pytest.mark.skipif(
        sys.platform == "win32" and not is_admin(),
        reason="Requires admin privileges",
    ), pytest.mark.tier_3, pytest.mark.timeout(3600)]

INSTALL_DIR = os.path.join(
    os.environ.get("ProgramFiles", r"C:\Program Files"), "Fauna",
)

# WinUI XAML must be published with MSBuild, NOT `dotnet build/publish`: on win-arm64
# `dotnet` invokes the XAML compiler out-of-process under x86 emulation and it crashes
# (conftest._build_windows_app uses the same path for the Debug build). Absent (e.g.
# an x64 box, where there is no emulation), the publish falls back to `dotnet publish`.
_MSBUILD = r"C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\MSBuild\Current\Bin\MSBuild.exe"


def _get_repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    )
    path = result.stdout.strip()
    if sys.platform == "win32" and path.startswith("/"):
        path = path[1].upper() + ":" + path[2:]
    return os.path.normpath(path)


def _build_msi(repo):
    """Build the MSI installer from source. Returns the MSI path."""
    build_dir = os.path.join(repo, "build", "installer")
    stage_dir = os.path.join(build_dir, "stage", "arm64")
    msi_path = os.path.join(build_dir, "Fauna-Setup-arm64.msi")

    os.makedirs(stage_dir, exist_ok=True)
    os.makedirs(os.path.join(stage_dir, "icons"), exist_ok=True)
    os.makedirs(os.path.join(stage_dir, "app"), exist_ok=True)

    ws_dir = os.path.join(repo, "apps", "fauna-windows")

    # 0. Ensure WiX extensions are installed (version must match WiX v6)
    wix_ver = subprocess.run(
        ["wix", "--version"], capture_output=True, text=True,
    )
    ext_ver = wix_ver.stdout.strip().split("+")[0]
    ext_errors = []
    # Firewall.wixext is NOT optional: ServiceNest.wxs opens inbound TCP 443 for the
    # nest through <firewall:FirewallException>, so a build without this extension dies
    # with WIX0200 "unhandled extension element 'FirewallException'". It was added to
    # wix.json, Package.wxs's build comment and installer/README.md when the rule landed,
    # but not here, so this builder had been unable to link an MSI at all.
    for ext in ["WixToolset.Util.wixext", "WixToolset.UI.wixext",
                "WixToolset.Firewall.wixext"]:
        r = subprocess.run(
            ["wix", "extension", "add", "-g", f"{ext}/{ext_ver}"],
            capture_output=True, text=True, timeout=60,
        )
        if r.returncode != 0:
            ext_errors.append(f"{ext}/{ext_ver}: rc={r.returncode} "
                              f"stdout={r.stdout.strip()} stderr={r.stderr.strip()}")
    if ext_errors:
        pytest.fail(f"WiX extension install failed:\n" + "\n".join(ext_errors))

    # The prebuild staged this flavor last; anything else here would package the
    # wrong core without a word.
    require_release_test_flavor(repo)

    # 1. Publish the .NET app self-contained (skip if publish output already exists).
    # Two requirements for the bare-VM install target (installer/README.md steps 3-4):
    #   - Publish with MSBuild `-t:Publish`, NOT `dotnet publish` — on win-arm64 the
    #     latter crashes the WinUI XAML compiler under x86 emulation (see _MSBUILD).
    #   - Self-contained (`SelfContained` + `WindowsAppSDKSelfContained`) so the
    #     installed app launches on a throwaway VM with no .NET 10 / Windows App SDK
    #     runtime — exactly the box TestCalDAVImapServingAfterInstall installs onto.
    # `dotnet publish` fallback (still self-contained) for an x64 box with no VS MSBuild
    # (no emulation there, so `dotnet` is safe).
    csproj = os.path.join(ws_dir, "FaunaApp", "FaunaApp", "FaunaApp.csproj")
    app_publish = os.path.join(
        ws_dir, "FaunaApp", "FaunaApp",
        "bin", "Release", "net10.0-windows10.0.26100", "win-arm64", "publish",
    )
    app_exe = os.path.join(app_publish, "FaunaApp.exe")
    # Republish when any input is newer than the published exe — never reuse a
    # publish merely because it exists, or a cached-MSI rebuild after a C#/XAML
    # change (or a fauna-ffi rebuild, whose MSVC DLL the publish bundles) ships
    # the OLD app and the journey tests witness it.
    # max_mtime skips gitignored output (bin/, obj/, the regenerated bindings);
    # the MSVC DLL lives in the gitignored target/, so it is read on its own.
    ffi_dll = release_ffi_dll(repo)
    publish_inputs_mtime = max(
        max_mtime([
            os.path.join(ws_dir, "FaunaApp", "**", "*.cs"),
            os.path.join(ws_dir, "FaunaApp", "**", "*.xaml"),
            os.path.join(ws_dir, "FaunaApp", "**", "*.csproj"),
        ]),
        os.path.getmtime(ffi_dll) if os.path.exists(ffi_dll) else 0.0,
    )
    if (not os.path.exists(app_exe)
            or os.path.getmtime(app_exe) < publish_inputs_mtime):
        if os.path.exists(_MSBUILD):
            publish_cmd = [
                _MSBUILD, csproj, "-restore", "-t:Publish",
                "-p:Configuration=Release", "-p:RuntimeIdentifier=win-arm64",
                "-p:SelfContained=true", "-p:WindowsAppSDKSelfContained=true",
                # The automation surface is `#if DEBUG || FAUNA_E2E_AGENT` since
                # 2026-08-01 (testing.md convention 15), so a plain Release publish
                # ships NO test agent — and TestFullJourneyInstalledApp below drives
                # the installed product through exactly that agent. This opt-in
                # property is convention 15's sanctioned "explicit e2e build
                # flavor"; the shipped MSI (release.yml) never sets it.
                "-p:FaunaE2eAgent=true",
                "-verbosity:minimal",
            ]
        else:
            publish_cmd = [
                "dotnet", "publish", csproj, "-c", "Release", "-r", "win-arm64",
                "--self-contained", "-p:WindowsAppSDKSelfContained=true",
                "-p:FaunaE2eAgent=true",  # see the MSBuild branch above
            ]
        # Cold self-contained WinUI publish (restore + XAML compile + bundle the
        # Windows App SDK self-contained runtime).
        tool = "MSBuild -t:Publish" if os.path.exists(_MSBUILD) else "dotnet publish"
        run_build_step(publish_cmd, f"WinUI self-contained publish ({tool})")

    # 2. Stage binaries — each one the collection-time prebuild built
    # (`helpers/helper_bins.py`), replayed here and never built.
    for name in ["fauna-sync-agent", "fauna-nest-svc", "fauna-bridge-svc", "fauna_shell",
                 "fauna-tui", "fauna-cli"]:
        src = helper_bin(name)
        shutil.copy2(src, os.path.join(stage_dir, src.name))

    # Stage the Go MDA + its gnullvm runtime DLLs (built into the ROOT target/, NOT
    # the apps/fauna-windows workspace target). They ship beside fauna-bridge-svc.exe
    # in INSTALLFOLDER. The MDA's gnullvm fauna_ffi.dll deliberately shares the name
    # of the .NET app's MSVC fauna_ffi.dll (staged under app/) — same name, DIFFERENT
    # stage subdir → different MSI install dir, never collide (ServiceBridge.wxs).
    root_target = os.path.join(repo, "target")
    for fname in ["fauna-mail-bridge.exe", "fauna_ffi.dll", "libunwind.dll"]:
        src = os.path.join(root_target, fname)
        if not os.path.exists(src):
            pytest.fail(
                f"Expected MDA build artifact missing: {src} "
                "(just windows-mail-bridge-build should have staged it in target/)"
            )
        shutil.copy2(src, os.path.join(stage_dir, fname))

    # Stage icons — the reference staging is two copies (installer/README.md § 1):
    # EVERY shell-ext overlay icon, plus the app's own AppIcon.ico from the FaunaApp
    # project's Assets. Globbing rather than naming four files keeps this from drifting
    # again the next time an overlay is added; ARPPRODUCTICON needs AppIcon.ico
    # specifically (Package.wxs's <Icon Id="FaunaIcon" SourceFile="$(var.IconDir)AppIcon.ico">).
    # A missing icon is fatal, never skipped: the previous `if os.path.exists(...)`
    # silently staged nothing and let `wix build` fail much later with WIX0103.
    icon_dest = os.path.join(stage_dir, "icons")
    overlay_icons = glob.glob(os.path.join(ws_dir, "shell-ext", "src", "icons", "*.ico"))
    if not overlay_icons:
        pytest.fail(
            "No shell-ext overlay icons found under "
            f"{os.path.join(ws_dir, 'shell-ext', 'src', 'icons')} — the ShellExt feature "
            "ships them as components."
        )
    for src in overlay_icons:
        shutil.copy2(src, os.path.join(icon_dest, os.path.basename(src)))
    app_icon = os.path.join(ws_dir, "FaunaApp", "FaunaApp", "Assets", "AppIcon.ico")
    if not os.path.exists(app_icon):
        pytest.fail(
            f"App icon missing: {app_icon} — Package.wxs stages it as ARPPRODUCTICON "
            "(the same file FaunaApp.csproj's <ApplicationIcon> uses)."
        )
    shutil.copy2(app_icon, os.path.join(icon_dest, "AppIcon.ico"))

    # Stage .NET app
    if os.path.isdir(app_publish):
        app_dest = os.path.join(stage_dir, "app")
        shutil.rmtree(app_dest)
        shutil.copytree(app_publish, app_dest)

    # 2a. The three licence files Notices.wxs installs (shipped-notices.md § Per
    # channel, the Windows MSI row), by the recipe release.yml's assembly job runs:
    # after the publish (the NuGet leg reads the project.assets.json its restore
    # wrote), over the mail bridge staged above. A generator that refuses fails the
    # build here, with its verdict lines; the notices file itself is never read.
    notices_dir = os.path.join(stage_dir, "notices")
    # Forward slashes: the recipe's body is bash, which keeps a backslash literal
    # only by luck of what follows it.
    notices_out = run_build_step(
        ["just", "windows-msi-notices", notices_dir.replace("\\", "/"), "arm64",
         os.path.join(root_target, "fauna-mail-bridge.exe").replace("\\", "/")],
        "the MSI's licence and notices files (just windows-msi-notices)", cwd=repo,
    )
    # The two parts of the closure no restore names — the runtime packs this
    # publish copied and the bridge's libunwind.dll — read off the verdicts.
    faults = msi_notices_faults(notices_out, "win-arm64")
    if faults:
        pytest.fail("the MSI's notices leave out part of its closure:\n  " + "\n  ".join(faults)
                    + "\nthe generator's verdict lines:\n" + "\n".join(l for l in notices_out if l.startswith("shipped-notices")))

    # 2b. The ShellExt feature ships Fauna-Sparse.msix as a component
    # (ShellExt.wxs's $(var.SparsePackage)). It is ProcessorArchitecture="neutral" —
    # built ONCE and shared by every arch — by a dedicated dev-fleet packaging script
    # that is deliberately not in this tree (installer/README.md § 2. Build the sparse
    # package), so this builder consumes the prebuilt artifact and never tries to make
    # one. Without the -d below, `wix build` dies with WIX0150 "Undefined preprocessor
    # variable '$(var.SparsePackage)'"; fail here instead, naming what is missing and
    # where it comes from, exactly as the staged-binary checks above do.
    sparse_pkg = os.path.join(repo, "build", "installer", "Fauna-Sparse.msix")
    if not os.path.exists(sparse_pkg):
        pytest.fail(
            f"Sparse package missing: {sparse_pkg} — the ShellExt feature ships it as a "
            "component. It is built once (arch-neutral) by the dev-fleet packaging "
            "script described in apps/fauna-windows/installer/README.md § 2, which is "
            "not part of this tree; copy the artifact in rather than rebuilding it here."
        )

    # 3. Build MSI
    installer_dir = os.path.join(ws_dir, "installer")
    wxs_files = glob.glob(os.path.join(installer_dir, "*.wxs"))
    wxl_files = glob.glob(os.path.join(installer_dir, "*.wxl"))
    loc_args = []
    for wxl in wxl_files:
        loc_args.extend(["-loc", wxl])
    run_build_step(
        [
            "wix", "build", "-arch", "arm64",
            "-ext", "WixToolset.Util.wixext",
            "-ext", "WixToolset.UI.wixext",
            "-ext", "WixToolset.Firewall.wixext",
            "-o", msi_path,
        ] + wxs_files + loc_args + [
            "-d", f"SyncBin={os.path.join(stage_dir, 'fauna-sync-agent.exe')}",
            "-d", f"NestBin={os.path.join(stage_dir, 'fauna-nest-svc.exe')}",
            "-d", f"BridgeBin={os.path.join(stage_dir, 'fauna-bridge-svc.exe')}",
            "-d", f"MailBridgeBin={os.path.join(stage_dir, 'fauna-mail-bridge.exe')}",
            "-d", f"MailBridgeFfiDll={os.path.join(stage_dir, 'fauna_ffi.dll')}",
            "-d", f"MailBridgeLibunwindDll={os.path.join(stage_dir, 'libunwind.dll')}",
            "-d", f"ShellDll={os.path.join(stage_dir, 'fauna_shell.dll')}",
            # Every *.wxs is preprocessed whichever Feature links it, so a build
            # without TuiBin dies with WIX0150 even for an install that
            # deselects TerminalApp.
            "-d", f"TuiBin={os.path.join(stage_dir, 'fauna-tui.exe')}",
            "-d", f"CliBin={os.path.join(stage_dir, 'fauna-cli.exe')}",
            "-d", f"IconDir={os.path.join(stage_dir, 'icons')}\\",
            "-d", f"AppDir={os.path.join(stage_dir, 'app')}\\",
            "-d", f"NoticesDir={notices_dir}\\",
            "-d", f"SparsePackage={sparse_pkg}",
        ],
        # The link compresses a ~310 MB staged payload (the self-contained WinUI
        # publish) into a ~180 MB MSI: 71 s on an idle Windows host, far longer under the
        # box's ordinary multi-slot contention, which once blew a 300 s bound and
        # read as a broken installer. Unbounded like every step (run_build_step).
        "WiX build (wix build -arch arm64)", cwd=installer_dir,
    )

    return msi_path


@pytest.fixture(scope="session")
def msi_path(request):
    """Build or return cached MSI installer. The publish and the link run here;
    their inputs were built at collection time, for a run that owes this build
    (`helpers/windows_installer.py::msi_build_owed`, the same two answers read
    below), so a cache hit needs none of them."""
    repo = _get_repo_root()
    msi, stamp = msi_paths(repo)
    current_mtime = source_mtime(repo)
    if msi_is_cached(repo, current_mtime):
        return msi

    # Cache miss — build
    request.getfixturevalue("mail_bridge_binary")
    result = _build_msi(repo)

    # Write stamp
    os.makedirs(os.path.dirname(stamp), exist_ok=True)
    with open(stamp, "w") as f:
        f.write(str(current_mtime))

    return result


# ── Cleanup fixture: remove leftover installation from interrupted runs ──

@pytest.fixture(scope="session", autouse=True)
def _cleanup_stale_install(msi_path):
    """Uninstall any leftover Fauna installation before running tests."""
    # Try MSI uninstall first
    subprocess.run(
        ["msiexec", "/x", msi_path, "/qn", "/norestart"],
        capture_output=True,  # unbounded: see _MSIEXEC_UNBOUNDED
    )
    # Force-remove any stale services that survived a bad uninstall
    for svc in ["FaunaNest", "FaunaBridge"]:
        subprocess.run(["sc", "stop", svc], capture_output=True)
        subprocess.run(["sc", "delete", svc], capture_output=True)
    # Remove install directory to clear orphaned files from old installs
    if os.path.isdir(INSTALL_DIR):
        shutil.rmtree(INSTALL_DIR, ignore_errors=True)
    time.sleep(2)
    yield


# ── Helpers ──

# _MSIEXEC_UNBOUNDED: no msiexec call here carries a wall-clock `timeout=`. The
# install runs inside the Windows Installer SERVICE, so killing the msiexec client
# on a timeout never stopped it, and the bound bought nothing. It only turned a
# slow success into a red: measured 2026-09-27 on a busy Windows host, the journey's
# `/x` took 140 s against a 120 s bound and raised TimeoutExpired in teardown while
# the uninstall completed cleanly behind it (install 66 s). A genuinely wedged run is still bounded by
# the per-test pytest timeout.

def msi_install(msi, features=None, log_suffix=""):
    """Install the MSI with optional feature selection."""
    cmd = ["msiexec", "/i", msi, "/qn", "/norestart"]
    if features is not None:
        # INSTALLLEVEL=0 disables all default features; ADDLOCAL then
        # explicitly enables only the ones we want.
        cmd.append("INSTALLLEVEL=0")
        cmd.append(f"ADDLOCAL={','.join(features)}")
    log_path = os.path.join(os.path.dirname(msi), f"install{log_suffix}.log")
    cmd.extend(["/l*v", log_path])
    result = subprocess.run(cmd, capture_output=True)  # see _MSIEXEC_UNBOUNDED
    return result.returncode, log_path


def msi_uninstall(msi, remove_data=False, log_suffix="", scrub=True):
    """Uninstall the MSI and clean up residual files/services.

    `scrub=False` leaves %ProgramFiles%\\Fauna as msiexec left it, for a test
    whose subject is what the uninstall itself removes; it then calls
    `scrub_install_dir()` once it has looked."""
    cmd = ["msiexec", "/x", msi, "/qn", "/norestart"]
    if remove_data:
        cmd.append("REMOVE_USER_DATA=1")
    log_path = os.path.join(os.path.dirname(msi), f"uninstall{log_suffix}.log")
    cmd.extend(["/l*v", log_path])
    result = subprocess.run(cmd, capture_output=True)  # see _MSIEXEC_UNBOUNDED
    # Force-stop and deregister services (msiexec may leave them)
    for svc in ["FaunaNest", "FaunaBridge"]:
        subprocess.run(["sc", "stop", svc], capture_output=True)
        subprocess.run(["sc", "delete", svc], capture_output=True)
    if scrub:
        scrub_install_dir()
    return result.returncode, log_path


def scrub_install_dir():
    """Remove the install directory (auto-harvested files may persist)."""
    if os.path.isdir(INSTALL_DIR):
        shutil.rmtree(INSTALL_DIR, ignore_errors=True)
    time.sleep(2)


def is_service_installed(service_name):
    """Check if a Windows service exists."""
    result = subprocess.run(
        ["sc", "query", service_name], capture_output=True, text=True,
    )
    return result.returncode == 0


def wait_for_service(service_name, timeout=10):
    """Poll until service is running or timeout. Returns True if running."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = subprocess.run(
            ["sc", "query", service_name], capture_output=True, text=True,
        )
        if "RUNNING" in result.stdout:
            return True
        time.sleep(1)
    return False


#: The three licence files every install carries beside the binaries (Notices.wxs;
#: shipped-notices.md § Per channel, the Windows MSI row).
LICENCE_FILES = ("LICENSE-MIT.txt", "LICENSE-APACHE.txt", "THIRD-PARTY-NOTICES.txt")


def file_installed(relative_path):
    """Check if a file exists under %ProgramFiles%\\Fauna."""
    return os.path.exists(os.path.join(INSTALL_DIR, relative_path))


#: The Sync feature's per-user logon launch (`ServiceSync.wxs`; installers/windows.md
#: § Feature Tree): an HKLM `…\Run` value that starts the agent in each user's session
#: under their standard token. There is no `FaunaSync` machine service to query.
_SYNC_RUN_KEY = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run"
_SYNC_RUN_VALUE = "Fauna Sync"


def sync_run_value():
    """The installed "Fauna Sync" logon-launch command, or None when it is absent."""
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, _SYNC_RUN_KEY, 0,
                            winreg.KEY_READ | winreg.KEY_WOW64_64KEY) as key:
            value, _kind = winreg.QueryValueEx(key, _SYNC_RUN_VALUE)
    except FileNotFoundError:
        return None
    return value


def assert_sync_logon_launch_registered():
    """The Sync feature is in: its Run value names the installed agent, and no
    `FaunaSync` machine service exists beside it."""
    expected = os.path.join(INSTALL_DIR, "fauna-sync-agent.exe")
    value = sync_run_value()
    assert value is not None, f"HKLM Run value {_SYNC_RUN_VALUE!r} missing after install"
    assert os.path.normcase(value) == os.path.normcase(expected), \
        f"HKLM Run value {_SYNC_RUN_VALUE!r} is {value!r}, expected {expected!r}"
    assert not is_service_installed("FaunaSync"), \
        "a FaunaSync machine service exists; the agent is a per-user logon launch"


def shell_dll_installed():
    """True if a version-stamped shell DLL (fauna_shell_<ver>.dll) is installed under
    %ProgramFiles%\\Fauna. Path 1 (installers/windows.md § Shell Extension) installs the
    DLL under a version-stamped @Name, so the bare fauna_shell.dll never exists on disk."""
    import glob as _glob
    return bool(_glob.glob(os.path.join(INSTALL_DIR, "fauna_shell_*.dll")))


class TestWinUIOnly:
    """Install only the desktop app — no services, no shell extension."""

    def test_install_winui_only(self, msi_path):
        rc, log = msi_install(msi_path, features=["DesktopApp"], log_suffix="_winui")
        assert rc == 0, f"Install failed (rc={rc}). See {log}"

        try:
            # App should be installed
            assert file_installed(os.path.join("App", "FaunaApp.exe")), \
                "FaunaApp.exe missing"

            # No machine services and no sync logon launch
            assert sync_run_value() is None, \
                "the Sync logon launch should not be registered"
            assert not is_service_installed("FaunaNest"), \
                "FaunaNest should not be installed"
            assert not is_service_installed("FaunaBridge"), \
                "FaunaBridge should not be installed"

            # No service binaries
            assert not file_installed("fauna-sync-agent.exe"), \
                "fauna-sync-agent.exe should not exist"
            assert not file_installed("fauna-nest-svc.exe"), \
                "fauna-nest-svc.exe should not exist"
            assert not file_installed("fauna-ctl.exe"), \
                "fauna-ctl.exe should not exist"
            assert not shell_dll_installed(), \
                "shell DLL (fauna_shell_<ver>.dll) should not exist"

        finally:
            rc, log = msi_uninstall(msi_path, log_suffix="_winui")
            assert rc == 0, f"Uninstall failed (rc={rc}). See {log}"

        # Verify cleanup
        assert not file_installed(os.path.join("App", "FaunaApp.exe")), \
            "FaunaApp.exe still exists after uninstall"


class TestNestOnly:
    """Install only the nest service — no sync, no app, no shell extension."""

    def test_install_nest_only(self, msi_path):
        rc, log = msi_install(msi_path, features=["Nest"], log_suffix="_nest")
        assert rc == 0, f"Install failed (rc={rc}). See {log}"

        try:
            # Nest service should be installed and running
            assert is_service_installed("FaunaNest"), \
                "FaunaNest not found after install"
            assert file_installed("fauna-nest-svc.exe"), \
                "fauna-nest-svc.exe missing"
            assert wait_for_service("FaunaNest"), \
                "FaunaNest not running after install"

            # fauna-ctl is deleted (installers/windows.md § Feature Tree)
            assert not file_installed("fauna-ctl.exe"), "fauna-ctl.exe should not exist"

            # No other service and no sync logon launch
            assert not is_service_installed("FaunaBridge"), \
                "FaunaBridge should not be installed"
            assert sync_run_value() is None, \
                "the Sync logon launch should not be registered"

            # No sync, app, or shell extension
            assert not file_installed("fauna-sync-agent.exe"), \
                "fauna-sync-agent.exe should not exist"
            assert not file_installed(os.path.join("App", "FaunaApp.exe")), \
                "FaunaApp.exe should not exist"
            assert not shell_dll_installed(), \
                "shell DLL (fauna_shell_<ver>.dll) should not exist"

            # The licence files ride every top-level feature (Notices.wxs), so the
            # headless subset carries them too — present and not empty, by size
            # alone (the notices file is dependency-authored text).
            for name in LICENCE_FILES:
                path = os.path.join(INSTALL_DIR, name)
                assert os.path.isfile(path) and os.path.getsize(path) > 0, \
                    f"{name} missing or empty beside the binaries of a nest-only install"

        finally:
            rc, log = msi_uninstall(msi_path, log_suffix="_nest")
            assert rc == 0, f"Uninstall failed (rc={rc}). See {log}"

        # Verify cleanup
        assert not is_service_installed("FaunaNest"), \
            "FaunaNest still exists after uninstall"
        assert not file_installed("fauna-nest-svc.exe"), \
            "fauna-nest-svc.exe still exists after uninstall"
        for name in LICENCE_FILES:
            assert not file_installed(name), f"{name} still exists after uninstall"


class TestSyncOnly:
    """Install only the sync agent — no nest, no app, no shell extension."""

    def test_install_sync_only(self, msi_path):
        rc, log = msi_install(msi_path, features=["Sync"], log_suffix="_sync")
        assert rc == 0, f"Install failed (rc={rc}). See {log}"

        try:
            # The agent and its per-user logon launch
            assert file_installed("fauna-sync-agent.exe"), \
                "fauna-sync-agent.exe missing"
            assert_sync_logon_launch_registered()

            # fauna-ctl is deleted (installers/windows.md § Feature Tree)
            assert not file_installed("fauna-ctl.exe"), "fauna-ctl.exe should not exist"

            # No other services
            assert not is_service_installed("FaunaNest"), \
                "FaunaNest should not be installed"
            assert not is_service_installed("FaunaBridge"), \
                "FaunaBridge should not be installed"

            # No app or shell extension
            assert not file_installed(os.path.join("App", "FaunaApp.exe")), \
                "FaunaApp.exe should not exist"
            assert not shell_dll_installed(), \
                "shell DLL (fauna_shell_<ver>.dll) should not exist"

        finally:
            rc, log = msi_uninstall(msi_path, log_suffix="_sync")
            assert rc == 0, f"Uninstall failed (rc={rc}). See {log}"

        # Verify cleanup
        assert sync_run_value() is None, \
            "the Sync logon launch is still registered after uninstall"
        assert not file_installed("fauna-sync-agent.exe"), \
            "fauna-sync-agent.exe still exists after uninstall"


class TestAllFeatures:
    """Install all features, verify everything, uninstall."""

    def test_install_all_features(self, msi_path):
        rc, log = msi_install(
            msi_path,
            features=["Sync", "ShellExt", "TerminalApp", "Nest", "Bridge", "DesktopApp"],
            log_suffix="_all",
        )
        assert rc == 0, f"Install failed (rc={rc}). See {log}"

        try:
            # Both machine services installed and running, the sync agent's
            # logon launch registered
            for svc in ["FaunaNest", "FaunaBridge"]:
                assert is_service_installed(svc), f"{svc} not found"
                assert wait_for_service(svc), f"{svc} not running"
            assert_sync_logon_launch_registered()

            # All binaries should be present
            assert file_installed("fauna-sync-agent.exe"), "fauna-sync-agent.exe missing"
            assert file_installed("fauna-tui.exe"), "fauna-tui.exe missing"
            assert file_installed("fauna-cli.exe"), "fauna-cli.exe missing"
            assert file_installed("fauna-nest-svc.exe"), "fauna-nest-svc.exe missing"
            assert file_installed("fauna-bridge-svc.exe"), "fauna-bridge-svc.exe missing"
            assert not file_installed("fauna-ctl.exe"), "fauna-ctl.exe should not exist"
            assert shell_dll_installed(), "shell DLL (fauna_shell_<ver>.dll) missing"
            assert file_installed(os.path.join("App", "FaunaApp.exe")), \
                "FaunaApp.exe missing"

            # Shell extension icons
            for ico in ["synced.ico", "syncing.ico", "cloud.ico", "error.ico"]:
                assert file_installed(ico), f"{ico} missing"

        finally:
            rc, log = msi_uninstall(msi_path, log_suffix="_all")
            assert rc == 0, f"Uninstall failed (rc={rc}). See {log}"

        # Verify cleanup
        for svc in ["FaunaNest", "FaunaBridge"]:
            assert not is_service_installed(svc), f"{svc} still exists after uninstall"
        assert sync_run_value() is None, \
            "the Sync logon launch is still registered after uninstall"
        assert not file_installed("fauna-sync-agent.exe"), \
            "fauna-sync-agent.exe still exists after uninstall"


_MACHINE_ENV_KEY = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"


def _machine_path_entries():
    """The machine `Path` as a NEW shell reads it: HKLM, expanded.

    Never os.environ: this process's PATH was fixed when it started, before the
    install, so it can neither see the entry the install writes nor lose it on
    uninstall — a check against it proves nothing either way."""
    import winreg
    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, _MACHINE_ENV_KEY) as key:
        raw, _ = winreg.QueryValueEx(key, "Path")
    return [os.path.expandvars(p) for p in raw.split(";") if p]


def _same_dir(a, b):
    """normpath drops the trailing separator `[INSTALLFOLDER]` carries."""
    return os.path.normcase(os.path.normpath(a)) == os.path.normcase(os.path.normpath(b))


def _fresh_shell(command, path):
    """Run `command` in cmd.exe with PATH set to `path`: the resolution a user's
    new shell does after the install, not this process's stale one."""
    env = dict(os.environ)
    env["PATH"] = path
    cmd_exe = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "System32", "cmd.exe")
    return subprocess.run(
        [cmd_exe, "/d", "/c", command], env=env,
        capture_output=True, text=True, timeout=120,
    )


class TestTerminalAppOnly:
    """Install only the terminal app: fauna-tui.exe + fauna-cli.exe on the machine PATH.

    The feature's promises (TerminalApp.wxs; installers/windows.md § Feature
    Tree; installers/tui.md § Decisions, decision 4): both binaries land in
    INSTALLFOLDER — the directory the Sync feature puts the agent in
    (ServiceSync.wxs), where the terminal app resolves it as a sibling; the one
    machine `Path` entry for that directory makes both typeable in any new
    shell; the uninstall removes the files and the entry.

    TerminalApp alone, without Sync: a TerminalApp-only install is legal
    (TerminalApp.wxs), and Sync's uninstall runs `CleanSyncRoots`, which
    unregisters EVERY Fauna sync root on the machine, sibling sessions' included.
    fauna-tui is only resolved, never launched: it has no headless invocation.
    """

    def test_install_terminal_app_only(self, msi_path):
        assert not any(_same_dir(p, INSTALL_DIR) for p in _machine_path_entries()), (
            f"the machine Path holds {INSTALL_DIR} BEFORE the install (a leftover "
            "from an earlier run?), so the uninstall half could not tell removal "
            "from survival"
        )
        rc, log = msi_install(msi_path, features=["TerminalApp"], log_suffix="_terminal")
        assert rc == 0, f"Install failed (rc={rc}). See {log}"

        try:
            for exe in ("fauna-tui.exe", "fauna-cli.exe"):
                assert file_installed(exe), f"{exe} missing from {INSTALL_DIR}"
            assert not file_installed("fauna-sync-agent.exe"), \
                "fauna-sync-agent.exe should not exist (Sync was not selected)"
            assert not file_installed(os.path.join("App", "FaunaApp.exe")), \
                "FaunaApp.exe should not exist"

            entries = _machine_path_entries()
            assert any(_same_dir(p, INSTALL_DIR) for p in entries), (
                f"the machine Path does not hold {INSTALL_DIR} after the install: "
                f"{entries}"
            )
            fresh = os.pathsep.join(entries)
            for name in ("fauna-tui", "fauna-cli"):
                found = shutil.which(name, path=fresh)
                assert found and _same_dir(os.path.dirname(found), INSTALL_DIR), (
                    f"{name} resolves to {found!r} through the fresh machine Path, "
                    f"expected {INSTALL_DIR}"
                )

            # A shell started after the install finds both by bare name.
            where = _fresh_shell("where fauna-tui", fresh)
            assert where.returncode == 0, (
                f"a fresh shell cannot find fauna-tui (rc={where.returncode}):\n"
                f"{where.stdout}{where.stderr}"
            )
            first = where.stdout.splitlines()[0]
            assert _same_dir(os.path.dirname(first), INSTALL_DIR), where.stdout
            # `--help` needs no host, account or store (the argv shell's
            # overview, libs/fauna-cli/src/argv.rs), and the launch catches a
            # binary that lands but does not start.
            cli = _fresh_shell("fauna-cli --help", fresh)
            assert cli.returncode == 0, (
                f"`fauna-cli --help` in a fresh shell exited {cli.returncode}:\n"
                f"stdout:\n{cli.stdout}\nstderr:\n{cli.stderr}"
            )
            assert "commands:" in cli.stdout and "mcp" in cli.stdout, (
                f"`fauna-cli --help` printed something other than the device "
                f"shell's overview:\n{cli.stdout}"
            )
        finally:
            # scrub=False: what the uninstall itself removes is under test.
            rc, log = msi_uninstall(msi_path, log_suffix="_terminal", scrub=False)

        try:
            assert rc == 0, f"Uninstall failed (rc={rc}). See {log}"
            for exe in ("fauna-tui.exe", "fauna-cli.exe"):
                assert not file_installed(exe), f"{exe} survived the uninstall"
            entries = _machine_path_entries()
            assert not any(_same_dir(p, INSTALL_DIR) for p in entries), (
                f"the machine Path still holds {INSTALL_DIR} after the uninstall: "
                f"{entries}"
            )
        finally:
            scrub_install_dir()


class TestUninstallPreservesData:
    """Uninstall without REMOVE_USER_DATA should preserve config."""

    def test_uninstall_preserves_data(self, msi_path):
        rc, _ = msi_install(msi_path, features=["Sync"], log_suffix="_data")
        assert rc == 0, "Install failed"

        # Create a marker file that should survive uninstall
        config_dir = os.path.join(
            os.environ.get("ProgramData", r"C:\ProgramData"), "Fauna",
        )
        os.makedirs(config_dir, exist_ok=True)
        marker = os.path.join(config_dir, "test-marker.txt")
        with open(marker, "w") as f:
            f.write("preserve me")

        try:
            rc, _ = msi_uninstall(msi_path, remove_data=False, log_suffix="_data")
            assert rc == 0, "Uninstall failed"

            assert os.path.exists(marker), "Config data was removed during uninstall"
        finally:
            if os.path.exists(marker):
                os.remove(marker)


# ── Installed CalDAV + IMAP serving (Track D increment 3) ─────────────────────
#
# The install-LEVEL proof for docs/goal/behavior/caldav-server.md § Process
# topology / § Network exposure (Desktop / IP). Its non-disruptive twin is
# test_caldav_imap_serving.py (temp dirs, ephemeral loopback ports, spawns the
# binaries directly — bypassing the SCM + supervisor). THIS drives the REAL MSI
# deploy: the FaunaNest + FaunaBridge virtual-account SCM services, the
# FaunaBridge-supervised MDA child, and the fixed installed ports. A perMachine
# install is disruptive on a shared box → throwaway VM / solo only (the module
# skipif(not admin) gate + installers/windows.md § Testing).

# Fixed ports of the installed deployment (no ephemeral allocation — a solo box).
# The SCM service binds the external 0.0.0.0:443 AND the fixed same-box TLS
# loopback 127.0.0.1:3000 (`fauna_protocol::node_policy::
# CANONICAL_INTERNAL_LOOPBACK_PORT`, service.rs `internal_loopback_port`) — the
# one a co-located client dials (nest/common.md § Same-box reach), so the test
# dials it too, over TLS pinned to the floor. 7450 is only `--foreground`'s
# `--port` default (main.rs); the service never binds it.
INSTALLED_NEST_PORT = 3000
INSTALLED_CALDAV_PORT = 8443    # fauna_mda_supervisor::CALDAV_LISTEN → operator-hatch
INSTALLED_IMAPS_PORT = 993      # MDA default imap_listen_implicit_tls (config.go)
# The admin-changed CalDAV port the supervisor must rebind the MDA to. Fixed (not
# find_free_port) to match this module's "no ephemeral allocation — a solo box"
# stance; the data dir is wiped + services stopped before the run, so nothing
# lingers on it. 9443 mirrors the mda_supervisor.rs unit-test example port.
INSTALLED_CALDAV_PORT_REBOUND = 9443


def _installed_nest_data_dir():
    r"""%ProgramData%\Fauna\nest — fauna-nest-service::config::default_data_dir."""
    base = os.environ.get("ProgramData", r"C:\ProgramData")
    return os.path.join(base, "Fauna", "nest")


def _assert_caldav_port_dead(port, timeout=60.0):
    """Poll until an HTTPS request to ``127.0.0.1:<port>/caldav/`` is REFUSED — the
    old CalDAV listener is gone after the supervisor's rebind restart. A
    ``ConnectionError`` (refused / reset) is the success signal; any HTTP/TLS
    response means something is still serving there. Ported from
    ``tests/test_caldav_admin_port_rebind.py::_assert_port_dead`` (the Docker twin
    of this assertion)."""
    import requests
    import urllib3

    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    url = f"https://127.0.0.1:{port}/caldav/"
    deadline = time.monotonic() + timeout
    last = "(never attempted)"
    while time.monotonic() < deadline:
        try:
            r = requests.request("PROPFIND", url, timeout=5.0, verify=False)
            last = f"still serving: HTTP {r.status_code}"
        except requests.exceptions.ConnectionError:
            return  # refused — the old listener is gone
        except requests.exceptions.RequestException as e:
            last = type(e).__name__
        time.sleep(1.0)
    pytest.fail(
        f"old CalDAV port {port} was still reachable {timeout:.0f}s after the admin "
        f"port change ({last}) — the FaunaBridge supervisor did not move the MDA off "
        "the old port"
    )


class TestCalDAVImapServingAfterInstall:
    """A real MSI-installed Windows nest serves CalDAV + IMAP end-to-end, and
    rebinds CalDAV to an admin-changed port via the FaunaBridge supervisor.

    Install the MSI's Nest + Bridge features → the FaunaNest + FaunaBridge SCM
    services start → claim + enable mail and CalDAV over WS-RPC + mint a
    known-password MUA credential (the headless equivalents of the client UI's
    claim + app-password flow, via the test-only seal-helper) → the
    FaunaBridge-supervised MDA child auto-enrolls on loopback (AUTO-APPROVED because
    mail/CalDAV is enabled — bins/fauna-nest/src/bridge_blob_handlers.rs
    request_enrollment auto-approve, BridgeRole::Mda => any mail/DAV enable) and
    self-provisions its x25519 (register_service_user → upsert_bridge_x25519), so the
    test does **no** manual register/approve/x25519-poke — the standalone engine's
    Stage C is the SCM service's job here. The MDA fetches the self-signed floor cert
    and binds CalDAV :8443 + IMAP :993 → two Python CalDAVClients bidirectionally
    round-trip a calendar event, a third dials the box's own LAN address on :8443
    (the bare address a home nest is reached at — never loopback) and round-trips
    with them, and a stock imaplib client opens INBOX.

    Only this environment exercises the SCM virtual-account services + the supervisor
    → MDA spawn → loopback auto-enrollment path (test_caldav_imap_serving.py spawns
    the binaries directly, bypassing the SCM + supervisor). It is the
    "installed-service function" coverage installers/windows.md § Testing § Coverage
    gaps flagged as missing.

    Transport — the NEST leg (claim, WS-RPC admin calls, credential mint, the port
    rebind) rides ``https``/``wss`` to the service's same-box loopback
    ``127.0.0.1:3000`` with that authority pinned to the floor cert's SPKI, read
    off ``<data dir>\\acme\\fullchain.pem`` (the engine's ``serve_tls``; its
    non-installed rehearsal is test_caldav_imap_serving.py::
    test_windows_nest_provisions_over_its_pinned_tls_floor) — no plain-HTTP or
    verification-off path to the installed nest. OUT OF SCOPE of that rule: the
    MUA legs (``verify=False`` / ``CERT_NONE`` to the MDA's CalDAV and IMAP
    listeners), which stand for third-party MUAs clicking through the floor by
    their own UX — the product's posture for them, not the harness's choice.

    IMAP binds because the engine enables mail explicitly beside CalDAV
    (`fauna.bridges.set_mail_enabled`): mail reads OFF on an unset toggle
    (mail-bridge-lifecycle.md § Default-off on first claim) and the MDA builds its
    IMAP listeners only when the snapshot says mail is on — the same call its
    binaries twin makes.
    """

    def test_installed_nest_serves_caldav_and_imap(self, msi_path):
        import imaplib
        import shutil as _shutil
        import ssl

        from helpers import windows_caldav_nest as eng
        from helpers.caldav_client import CalDAVClient, build_vevent
        from helpers.caldav_roundtrip import caldav_has, wait_caldav_serving

        from helpers.installed_nest_reach import lan_ipv4

        # Fail-fast SKIP (before the expensive install) if the test-only seal-helper
        # is not built — the headless credential mint needs it (no Fauna app UI).
        eng.seal_helper_exe()
        # The bare-address leg below dials the box's own LAN address, never a
        # loopback stand-in; a box with none cannot run it.
        lan = lan_ipv4()
        if lan is None:
            pytest.skip("this box has no LAN IPv4 address to reach the installed "
                        "CalDAV listener at")

        # A prior serving run leaves a CLAIMED nest under %ProgramData%\Fauna, but a
        # fresh claim needs a pristine data dir (the claim-code is consumed on first
        # claim). The session-autouse _cleanup_stale_install already stopped the
        # services + uninstalled, so the tree is quiescent — clear it for a clean
        # claim. (Solo / throwaway-VM only, per the module-level admin gate.)
        _shutil.rmtree(
            os.path.join(os.environ.get("ProgramData", r"C:\ProgramData"), "Fauna"),
            ignore_errors=True,
        )

        rc, log = msi_install(msi_path, features=["Nest", "Bridge"], log_suffix="_caldav")
        assert rc == 0, f"Install (Nest+Bridge) failed (rc={rc}). See {log}"

        prov = None
        try:
            # The SCM services start on install (ServiceControl Start="install").
            assert wait_for_service("FaunaNest", timeout=30), \
                "FaunaNest service not running after install"
            assert wait_for_service("FaunaBridge", timeout=30), \
                "FaunaBridge service not running after install"

            # Claim + enable-CalDAV + mint a known-password MUA credential against
            # the INSTALLED nest (its same-box TLS loopback :3000, %ProgramData%
            # data dir), every call pinned to the floor's SPKI (serve_tls).
            # nest_proc=None: the FaunaNest SCM service owns the process, so the
            # engine reads <data-dir>/claim-code rather than tracking a Popen.
            prov = eng.provision_caldav_on_running_nest(
                port=INSTALLED_NEST_PORT,
                nest_data_dir=_installed_nest_data_dir(),
                nest_proc=None,
                serve_tls=True,
            )

            # set_caldav_enabled (above) wrote the `caldav-enabled` flag into the nest
            # data dir; the FaunaBridge supervisor (15 s poll) spawns the MDA, which
            # self-enrolls on loopback (auto-approved), provisions its x25519, fetches
            # the floor cert, and binds CalDAV :8443 + IMAP :993. No standalone MDA
            # spawn and NO manual enroll/poke here — the installed service does it.
            caldav_base = f"https://127.0.0.1:{INSTALLED_CALDAV_PORT}"
            wait_caldav_serving(caldav_base, verify=False, timeout=180.0)

            # ── CalDAV bidirectional round-trip via two MUAs on the shared calendar.
            mua_a = CalDAVClient(caldav_base, prov.auth_username, prov.password, verify=False)
            mua_b = CalDAVClient(caldav_base, prov.auth_username, prov.password, verify=False)
            cal_a = mua_a.personal_calendar()
            cal_b = mua_b.personal_calendar()

            nonce = os.urandom(5).hex()
            s_a, uid_a = f"{nonce}-from-a", f"{nonce}-uid-a"
            mua_a.put_event(cal_a, uid_a, build_vevent(
                uid_a, s_a, "20260620T120000Z", "20260620T130000Z"))
            assert caldav_has(mua_b, cal_b, s_a, timeout=60.0), (
                f"event {s_a!r} created by MUA A never became visible to MUA B against "
                "the INSTALLED MDA (CalDAV serving/persist/sync break)"
            )
            s_b, uid_b = f"{nonce}-from-b", f"{nonce}-uid-b"
            mua_b.put_event(cal_b, uid_b, build_vevent(
                uid_b, s_b, "20260620T140000Z", "20260620T150000Z"))
            assert caldav_has(mua_a, cal_a, s_b, timeout=60.0), (
                f"event {s_b!r} created by MUA B never became visible to MUA A"
            )

            # ── The bare address: a calendar app elsewhere on the network dials the
            # box's own LAN address on the default CalDAV port — the supervisor
            # binds the MDA's listener on every interface (`fauna_mda_supervisor::
            # CALDAV_LISTEN`, caldav-server.md § Network exposure, Desktop / IP).
            # Same credential, same calendar: what A wrote on loopback reads back
            # here, and what is written here reads back on loopback.
            lan_base = f"https://{lan}:{INSTALLED_CALDAV_PORT}"
            wait_caldav_serving(lan_base, verify=False, timeout=60.0)
            mua_lan = CalDAVClient(lan_base, prov.auth_username, prov.password, verify=False)
            cal_lan = mua_lan.personal_calendar()
            assert caldav_has(mua_lan, cal_lan, s_a, timeout=60.0), (
                f"event {s_a!r} never became visible at the box's LAN address "
                f"{lan_base} — the installed CalDAV listener does not serve on a bare "
                "address"
            )
            s_l, uid_l = f"{nonce}-from-lan", f"{nonce}-uid-lan"
            mua_lan.put_event(cal_lan, uid_l, build_vevent(
                uid_l, s_l, "20260620T150000Z", "20260620T160000Z"))
            assert caldav_has(mua_a, cal_a, s_l, timeout=60.0), (
                f"event {s_l!r} written at the LAN address {lan_base} never became "
                "visible on loopback"
            )

            # ── IMAP: the same credential opens the mailbox (one MDA serves both).
            ctx = ssl.create_default_context()
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            imap = imaplib.IMAP4_SSL("127.0.0.1", INSTALLED_IMAPS_PORT, ssl_context=ctx)
            try:
                imap.login(prov.auth_username, prov.password)
                typ, _ = imap.select("INBOX")
                assert typ == "OK", f"IMAP SELECT INBOX failed: {typ}"
                typ_l, _ = imap.list()
                assert typ_l == "OK", f"IMAP LIST failed: {typ_l}"
            finally:
                try:
                    imap.logout()
                except Exception:
                    pass

            # ── Desktop CalDAV port-rebind: an admin changes the port from a client
            # → the REAL FaunaBridge service supervisor re-pins the MDA's
            # operator-hatch and restarts the MDA on the new port. The desktop twin
            # of test_caldav_admin_port_rebind.py, but here the supervisor (not the
            # test) does the restart: the desktop MDA is operator-hatch-pinned, so it
            # does NOT self-exit on config_changed (`CalDAVBindIsAdminPort` is false);
            # the supervisor's `caldav-port` flag watch (mda_supervisor.rs
            # RECONCILE_POLL=15s) is the SOLE rebind driver. Only this installed-
            # service path exercises that supervisor → MDA chain (the binaries-direct
            # test_caldav_imap_serving.py bypasses the supervisor). Authority:
            # caldav-server.md § Network exposure (Desktop / IP deployment).
            from clients.ws_rpc_admin_client import WsRpcAdminClient

            admin_sk = prov.admin["signing_key"]
            admin_ws = WsRpcAdminClient(
                prov.nest_url, actor_id=prov.actor_id, signing_key=bytes(admin_sk),
            )
            with admin_ws:
                admin_ws.call(
                    "fauna.bridges.set_caldav_port",
                    {"port": INSTALLED_CALDAV_PORT_REBOUND},
                )

            # The nest wrote <nest_data_dir>/caldav-port; the FaunaBridge supervisor
            # (≤15s poll, then a 5s RESTART_DELAY on the kill/respawn) re-pins the
            # operator-hatch to 0.0.0.0:<new> and restarts the MDA. Give it a generous
            # window (supervisor poll + the MDA's cold re-enroll/bind).
            rebound_base = f"https://127.0.0.1:{INSTALLED_CALDAV_PORT_REBOUND}"
            wait_caldav_serving(rebound_base, verify=False, timeout=180.0)

            # The OLD port must go dead — the supervisor killed the MDA that held it.
            _assert_caldav_port_dead(INSTALLED_CALDAV_PORT, timeout=60.0)

            # Event A (written on the OLD port) SURVIVES the rebind, and a fresh write
            # on the NEW port works — the round-trip is intact after the port change.
            mua_c = CalDAVClient(
                rebound_base, prov.auth_username, prov.password, verify=False)
            cal_c = mua_c.personal_calendar()
            assert caldav_has(mua_c, cal_c, s_a, timeout=60.0), (
                f"event {s_a!r} (written on the old port {INSTALLED_CALDAV_PORT}) must "
                f"survive the admin port change and be visible on the new port "
                f"{INSTALLED_CALDAV_PORT_REBOUND} (supervisor rebind / store-persist break)"
            )
            s_c, uid_c = f"{nonce}-from-c", f"{nonce}-uid-c"
            mua_c.put_event(cal_c, uid_c, build_vevent(
                uid_c, s_c, "20260620T160000Z", "20260620T170000Z"))
            assert caldav_has(mua_c, cal_c, s_c, timeout=60.0), (
                f"a fresh write {s_c!r} on the new CalDAV port "
                f"{INSTALLED_CALDAV_PORT_REBOUND} never became visible after the rebind"
            )
        finally:
            # Drop the :3000 pin with the nest it named (the SCM owns the process;
            # cleanup() stops nothing here). Throwaway-VM teardown: REMOVE_USER_DATA=1
            # deletes %ProgramData%\Fauna so a re-run claims fresh (the claimed nest
            # would otherwise block re-claim).
            if prov is not None:
                prov.cleanup()
            msi_uninstall(msi_path, remove_data=True, log_suffix="_caldav")


# ── Install-then-reach: the installed nest SERVES, seen from outside it ──────────
#
# TestNestOnly proves the FaunaNest service is installed and RUNNING; running is not
# serving, and a packaging check colours no catalog cell (packaging-witnesses.md
# § Implementation status today). This class is the channel's own install-then-
# reach test (installers/README.md § The channel contract), the Windows twin of
# tests/platform/macos/test_installer.py::TestInstallThenReach: install the MSI's
# Nest feature alone, read the claim code the service writes at its first start,
# then claim and read health at the box's own LAN address on the service's :443 —
# never loopback, never a spawned stand-in.
#
# Scope of a same-box dial: it proves the service binds every interface and serves
# its floor there, which is the half of "your other devices reach it over the
# network" a single box can observe. Windows does not filter a connection to the
# box's own address, so the Firewall inbound allow (ServiceNest.wxs) is not proved
# here; it stays authoring-guarded (installers/windows.md § Implementation status
# today, its own row).
#
# It carries no feature mark yet, as its macOS twin did: it becomes get-the-app
# outcome 3's windows witness (the mark + the citation, in one commit) only once an
# elevated run has seen it green — until then the outcome's witness stays (none).

#: The service's external serving bind (service.rs: `0.0.0.0:443` under the SCM).
INSTALLED_NEST_EXTERNAL_PORT = 443
#: How long the freshly installed service may take to write its claim code and
#: answer at the LAN address (first start runs migrations + mints the floor cert).
INSTALLED_NEST_READY_TIMEOUT = 120.0


def _installed_nest_diagnosis():
    """What a failure quotes (convention 6): the SCM's view of FaunaNest and the
    tail of the service's own log under the data dir (the SCM discards stdout)."""
    from pathlib import Path

    from helpers.windows_caldav_nest import _log_tail, nest_log_path

    q = subprocess.run(["sc", "queryex", "FaunaNest"], capture_output=True, text=True)
    log = nest_log_path(Path(_installed_nest_data_dir()))
    return (
        f"sc queryex FaunaNest (rc={q.returncode}):\n{(q.stdout or q.stderr)[-2000:]}\n"
        f"service log ({log}):\n{_log_tail(log)}"
    )


class TestInstallThenReach:
    """The MSI-installed nest is a network-reachable server: claimed and healthy at
    the box's LAN address on the FaunaNest service's :443 (installers/windows.md
    § Implementation status today, the Network-reachable nest, Claim-code bootstrap
    and Headless nest-only rows). The claim code is the one the service writes to
    `%ProgramData%\\Fauna\\nest\\claim-code` at its first start — the file a service
    with no console conveys it by."""

    def test_installed_nest_is_claimed_and_healthy_at_its_lan_address(self, msi_path):
        from pathlib import Path

        from common.auth import claim_admin, pin_nest_spki, unpin_nest_spki
        from helpers.installed_nest_reach import lan_ipv4, pinned_health
        from helpers.windows_caldav_nest import floor_cert_spki

        lan = lan_ipv4()
        if lan is None:
            pytest.skip("this box has no LAN IPv4 address to reach the nest at")

        data_dir = Path(_installed_nest_data_dir())
        claim_file = data_dir / "claim-code"
        # A claimed nest's data dir is a user's: never touched. An unclaimed one
        # (claim code still on disk — what TestNestOnly's plain uninstall leaves)
        # holds nothing but the service's own first-start state, so this test may
        # claim it and remove it again at teardown.
        if (data_dir / "nest.db").exists() and not claim_file.exists():
            pytest.fail(
                f"{data_dir} holds an already-CLAIMED nest; this test claims a fresh "
                "one and never touches a claimed nest's data. Remove it by hand on a "
                f"test box (an uninstall with REMOVE_USER_DATA=1, or delete {data_dir}) "
                "and re-run.")

        rc, log = msi_install(msi_path, features=["Nest"], log_suffix="_reach")
        assert rc == 0, f"Install (Nest) failed (rc={rc}). See {log}"

        base = f"https://{lan}:{INSTALLED_NEST_EXTERNAL_PORT}"
        authority = f"{lan}:{INSTALLED_NEST_EXTERNAL_PORT}"
        claimed = False
        try:
            assert wait_for_service("FaunaNest", timeout=30), (
                f"FaunaNest not running after install\n{_installed_nest_diagnosis()}")

            deadline = time.monotonic() + INSTALLED_NEST_READY_TIMEOUT
            while not claim_file.exists() and time.monotonic() < deadline:
                time.sleep(1)  # sleep-ok: poll interval of a deadline loop
            assert claim_file.exists(), (
                f"the installed service never wrote {claim_file} within "
                f"{INSTALLED_NEST_READY_TIMEOUT:.0f}s\n{_installed_nest_diagnosis()}")
            claim_code = claim_file.read_text().strip()

            # Trust the nest by the floor it says it serves — its own
            # acme\fullchain.pem — pinned to the LAN authority, so a leaf that
            # disagrees is refused before a byte is written.
            last = "(never attempted)"
            healthy = None
            while time.monotonic() < deadline:
                try:
                    pin_nest_spki(authority, floor_cert_spki(data_dir))
                    status, body = pinned_health(base)
                    if status == 200 and (body or {}).get("status") == "ok":
                        healthy = body
                        break
                    last = f"HTTP {status}: {body}"
                except (OSError, ValueError) as e:
                    last = f"{type(e).__name__}: {e}"
                time.sleep(1)  # sleep-ok: poll interval of a deadline loop
            assert healthy is not None, (
                f"the installed nest never answered health at {base} (the LAN "
                f"address on the service's :443) within "
                f"{INSTALLED_NEST_READY_TIMEOUT:.0f}s — last: {last}\n"
                f"{_installed_nest_diagnosis()}")
            # What answered is THIS install's nest: the pin is the leaf key of the
            # floor in its own data dir, so no other server on :443 passes it.

            admin = claim_admin(
                INSTALLED_NEST_EXTERNAL_PORT, claim_code, base_url=base, handle="test")
            claimed = True
            assert admin["token"], f"claim at {base} returned no bearer: {admin}"

            status, body = pinned_health(base)
            assert status == 200 and (body or {}).get("status") == "ok", (
                f"health at {base} after the claim: HTTP {status}: {body}\n"
                f"{_installed_nest_diagnosis()}")
            # The service consumes the claim code on claim (reconcile deletes it).
            deadline = time.monotonic() + 30
            while claim_file.exists() and time.monotonic() < deadline:
                time.sleep(1)  # sleep-ok: poll interval of a deadline loop
            assert not claim_file.exists(), \
                f"{claim_file} survived the claim — the nest did not record it"
        finally:
            unpin_nest_spki(authority)
            # Only a nest THIS test claimed has its data removed: it holds nothing
            # but the test's own claim, and a re-run needs a fresh claim code.
            msi_uninstall(msi_path, remove_data=claimed, log_suffix="_reach")


# ══════════════════════════════════════════════════════════════════════════════
# Full journey — the INSTALLED product, driven through the client UI only
# ══════════════════════════════════════════════════════════════════════════════
#
# Every other test in this file is headless: it installs the MSI, then reads
# files / services / registry. Every windows *UI* test elsewhere drives the DEV
# build over an in-memory pipe fake (`sync_inject_locations` →
# `FoldersPage.TestPipeOverride`) — `test_folders.py` says it outright: "there
# is no live fauna-sync-agent in e2e". So those two halves have never met, and
# the seam between them is exactly what broke live on 2026-07-17: the MSI
# shipped, the app ran, and the chain from "user binds a folder in the UI" to
# "the installed agent serves it" was silently dead.
#
# These tests cross that seam. The app under test is the MSI's own
# %ProgramFiles%\Fauna\App\FaunaApp.exe (the `installed_product` marker →
# conftest's `_installed_windows_app_override`), which resolves its agent as
# baseDir\..\fauna-sync-agent.exe (`App.xaml.cs` SpawnSyncAgentDetached) — so the
# binaries under test are the ones a user actually gets, and the bind runs over
# the REAL \\.\pipe\fauna-sync because we deliberately never seed the pipe fake.
#
# What is deliberately NOT re-proved here: the cfapi primitives. Population,
# hydrate-on-open, the Synced row flip, the badge push, and the local-edit upload
# are already pinned headlessly in ~0.5 s by
# `fauna-sync-agent::cfapi_live_integration` + `libs/fauna-cfapi`. Re-testing
# them through a multi-minute MSI journey would be slower AND weaker. The value
# here is only the links BETWEEN proven parts — the gap `file-sync.md`
# § Implementation status today names: "a tier_3 provision→bind→GetFileStatus
# harness over real binaries stays a deferred follow-on".
#
# WARNING — ADMIN + EXCLUSIVE BOX. `_cleanup_stale_install` / `msi_uninstall` run
# `msiexec /x`, `sc delete` the services and rmtree %ProgramFiles%\Fauna: they
# destroy whatever Fauna install any other session on this machine is using. The
# windows app is session-scoped (`_driver_cache`), so run this in its own pytest
# invocation.

import secrets  # noqa: E402

from helpers import windows_sync_agent as _wsa  # noqa: E402

_INSTALLED_AGENT = os.path.join(INSTALL_DIR, "fauna-sync-agent.exe")

#: Image paths of every running fauna-sync-agent.exe on this box. OBSERVE ONLY — never
#: kill: sibling dev sessions run their own agent out of their own build tree
#: (one was live while this was written), and killing another session's work is
#: never ours to do. Path-scoping every assertion is the same property that keeps
#: the MSI's own `KillFaunaSync` CA from reaping a developer's build-dir agent.
_running_sync_agent_paths = _wsa.running_sync_agent_paths


def _wait_for_installed_agent(timeout=60.0):
    """Poll until an agent running from INSTALL_DIR appears; return its path or None.

    The app spawns it lazily at login (`SpawnSyncAgentDetached`, best-effort), so
    it arrives a beat after the session comes up.

    Ownership is `windows_sync_agent.installed_agents` — a path-COMPONENT compare,
    so a same-prefix stranger (`...\\Fauna-old\\fauna-sync-agent.exe`) is never mistaken
    for the installed product's agent.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ours = _wsa.installed_agents(_running_sync_agent_paths(), INSTALL_DIR)
        if ours:
            return ours[0]
        time.sleep(0.5)
    return None


@pytest.fixture
def installed_product(msi_path):
    """The real MSI installed with the app + its sync agent; removed after.

    `DesktopApp` brings %ProgramFiles%\\Fauna\\App\\FaunaApp.exe; `Sync` brings the
    fauna-sync-agent.exe beside it that the app will spawn. Ordered BEFORE the app
    fixture by every test's signature — `_installed_windows_app_override` raises
    if the product is absent when the driver launches, so a mis-ordering fails
    loudly instead of silently testing the dev build.

    Guarded on the box precondition the whole journey rests on: no FOREIGN agent
    may be serving the per-user sync pipe. The app only spawns its own agent when
    that probe fails, so an intruder would silently become the subject under test
    — and be written into. Checked BEFORE the install, so a contaminated box costs
    a second rather than a 20-minute run and a wrong diagnosis.
    """
    blocker = _wsa.blocking_diagnosis(INSTALL_DIR)
    if blocker:
        pytest.fail(f"this suite needs an EXCLUSIVE box, and this one is not: {blocker}")

    rc, log = msi_install(
        msi_path, features=["DesktopApp", "Sync"], log_suffix="_journey"
    )
    assert rc == 0, f"MSI install failed (rc={rc}); see {log}"
    assert os.path.exists(_INSTALLED_AGENT), (
        f"{_INSTALLED_AGENT} missing after installing the Sync feature — the app "
        "would have nothing to spawn"
    )
    yield INSTALL_DIR
    msi_uninstall(msi_path, remove_data=True, log_suffix="_journey")


@pytest.fixture
def journey_app(installed_product, logged_in_app):
    """The INSTALLED app, logged in against a real nest.

    The fixture order IS the contract: `installed_product` must materialize the
    binaries before `logged_in_app` launches a driver against them.
    """
    return logged_in_app


@pytest.mark.windows
@pytest.mark.installed_product
# real_sync_agent → FAUNA_E2E_REAL_SYNC_AGENT=1 at app launch, so the set_state
# login starts the session-scoped HydrationSessionService (App.xaml.cs) — the
# spawn→provision→hydration chain is unreachable under an e2e login without it
# (the root cause: set_state never enters StartMainAppAsync, where the
# once-at-login provision used to live).
@pytest.mark.real_sync_agent
class TestFullJourneyInstalledApp:
    """installer → agent lifecycle → provisioning → bind → upload → Media."""

    @pytest.mark.feature("get-the-app")
    def test_installed_app_spawns_the_installers_own_sync_agent(self, journey_app):
        """The first link, and the one with no other witness: the INSTALLED app
        must bring up the INSTALLED agent.

        `SpawnSyncAgentDetached` probes \\\\.\\pipe\\fauna-sync.<SID> and, if
        unreachable, spawns its first existing candidate — baseDir\\..\\fauna-sync-agent.exe
        for an installed layout. Asserting the running agent's image path lies
        under %ProgramFiles%\\Fauna is what separates "the installed product works"
        from "some agent happens to be running on this dev box" — a distinction no
        existing test draws, and precisely the confusion the 2026-07-17 incident
        hid behind (a stale build-tree agent kept serving while the freshly
        installed one was never provisioned).
        """
        agent = _wait_for_installed_agent()
        assert agent is not None, (
            f"no fauna-sync-agent.exe running from {INSTALL_DIR} after login — the "
            "installed app never spawned the installer's agent. Agents seen "
            f"elsewhere (NOT ours; left alone): {_running_sync_agent_paths()}"
        )
        assert os.path.normcase(agent) == os.path.normcase(_INSTALLED_AGENT), (
            f"the running agent is {agent}, expected {_INSTALLED_AGENT}"
        )

    @pytest.mark.feature("get-the-app")
    def test_a_file_added_to_a_ui_bound_folder_appears_in_media(
        self, journey_app, tmp_path
    ):
        """The whole journey, as a user walks it.

        Create a folder in the wizard → bind a local folder from inside that
        set's row (typed path; the set is contextual, so there is no folder-name
        field — the removed `folder-location-fileset-input`) → drop a file into it →
        it appears on the Media page.

        Deliberately does NOT call `sync_inject_locations`: that swaps the page's VM
        onto the in-memory pipe fake (`FoldersPage.TestPipeOverride`), which is
        what every other windows bind test does and is exactly why none of them
        can see this chain. With the fake absent, the page builds a real
        `SyncServicePipeClient`, so the bind travels the real named pipe to the
        real installed agent and the upload is the real shared
        `fauna_sync_engine` watch→upload loop.
        """
        app = journey_app
        name = f"journey-{secrets.token_hex(4)}"

        b = app.backups
        b.navigate_folders()
        b.create_folder_via_wizard(name)
        idx = b.find_and_expand_folder(name)

        watched = tmp_path / "journey-src"
        watched.mkdir()
        app.driver.type_text("folder-location-path-input", str(watched))
        app.driver.click("folder-location-add-button")

        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1:
                break
            time.sleep(0.3)
        assert app.driver.count("folder-location-row", scope=f"folder-row[{idx}]") >= 1, (
            "the folder never bound over the REAL fauna-sync pipe; "
            f"error={app.error_text()!r}"
        )

        # A new file in a synced folder is adopted and uploaded — on-demand-files.md
        # § On-Demand Files: "New files are adopted." No setting suppresses it.
        (watched / "hello.txt").write_text(f"journey bytes {secrets.token_hex(8)}")

        app.media.navigate()
        app.media.set_filter(name)
        count = app.media.wait_for_item_count(1, timeout=90.0)
        assert count == 1, (
            f"the file never reached the Media page (count={count}) — the "
            "installer→agent→provision→upload→projection chain is broken "
            f"somewhere. error={app.error_text()!r}"
        )
        assert "hello.txt" in app.media.item_name(0), app.media.item_names()
