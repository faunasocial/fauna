"""E2E tests: Windows MSI installer — independent feature installation."""

import glob
import json
import os
import shutil
import subprocess
import sys
import time

import pytest


def _is_admin():
    """Check if the current process has admin privileges."""
    if sys.platform != "win32":
        return False
    import ctypes
    return ctypes.windll.shell32.IsUserAnAdmin() != 0


pytestmark = [pytest.mark.skipif(sys.platform != "win32", reason="Windows-only"), pytest.mark.skipif(
        sys.platform == "win32" and not _is_admin(),
        reason="Requires admin privileges",
    ), pytest.mark.tier_3]

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


def _git_ignored(paths):
    """The subset of `paths` git ignores — this tree's one definition of build output."""
    if not paths:
        return set()
    # -z: NUL-separated in and out. Without it git C-quotes every path holding a
    # backslash ("D:\\src\\…"), so no Windows path would ever match.
    r = subprocess.run(
        ["git", "check-ignore", "-z", "--stdin"], input="\0".join(paths) + "\0",
        capture_output=True, text=True, cwd=os.path.dirname(os.path.abspath(__file__)),
    )
    return {os.path.normcase(os.path.normpath(p)) for p in r.stdout.split("\0") if p}


def _max_mtime(patterns):
    """Return the maximum mtime across all files matching the glob patterns.

    Gitignored files are build OUTPUT and never count as input: `bin/`/`obj/`
    (the WinUI publish writes generated *.cs there), `Generated/uniffi/` and the
    test fakes step 1c regenerates. All of these are written AFTER `msi_path`
    takes the stamp, so counting them made its cache miss on every run following
    a build. (Tracked generated files such as `Generated/UiIds.cs` stay inputs.)
    """
    paths = [p for pattern in patterns for p in glob.glob(pattern, recursive=True)]
    ignored = _git_ignored(paths)
    best = 0.0
    for path in paths:
        if os.path.normcase(os.path.normpath(path)) in ignored:
            continue
        try:
            best = max(best, os.path.getmtime(path))
        except OSError:
            pass
    return best


def _run_build_step(cmd, what, cwd=None):
    """Run one installer build step to completion; `pytest.fail` with its tail on error.

    No wall-clock bound, the same shape as conftest's `_build_via_just`. Every
    step here is a compile or a link, and on Windows, which runs several build
    slots at once by design, a cold one legitimately outlasts any fixed bound.
    The old `subprocess.run(timeout=3600)` failed a build that was merely busy,
    and on Windows it could not even do that promptly: it kills only the
    immediate child, then blocks draining the pipe until the grandchild cargo
    exits. Measured 2026-09-26/27: TimeoutExpired raised at 6789 s, 7723 s and
    4257 s, with each build's result thrown away. Convention 9 is kept by `reap_descendants_of`: the whole tree dies
    with the run if pytest is reaped. `drain_pipes` keeps the pipe read for
    the child's life (convention 13) and holds the tail this failure quotes.
    """
    from drivers.port_util import (
        drain_pipes,
        popen_group_kwargs,
        reap_descendants_of,
        wait_pipes_drained,
    )

    print(f"[installer-build] {what}", flush=True)
    proc = subprocess.Popen(
        cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, bufsize=1, **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    recent = drain_pipes(proc, maxlen=400)
    rc = proc.wait()
    if rc != 0:
        wait_pipes_drained(proc, timeout=10.0)
        pytest.fail(f"{what} failed (rc={rc}):\n" + "\n".join(recent))


def _build_msi(repo):
    """Build the MSI installer from source. Returns the MSI path."""
    build_dir = os.path.join(repo, "build", "installer")
    stage_dir = os.path.join(build_dir, "stage", "arm64")
    msi_path = os.path.join(build_dir, "Fauna-Setup-arm64.msi")

    os.makedirs(stage_dir, exist_ok=True)
    os.makedirs(os.path.join(stage_dir, "icons"), exist_ok=True)
    os.makedirs(os.path.join(stage_dir, "app"), exist_ok=True)

    ws_dir = os.path.join(repo, "apps", "fauna-windows")
    cargo_cmd = os.path.join(repo, "scripts", "cargo-win.cmd")

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

    # 1. Build Rust binaries
    #
    # The sync agent first, in a cargo invocation of its OWN: it is the
    # cross-platform `fauna-sync-agent` package (the windows-only wrapper crate
    # that used to produce this exe is retired), and one invocation naming several
    # packages unifies their features — beside `-p fauna-tui` the agent's shared
    # dependencies would be compiled with tui's wider feature set, which is not
    # the binary release.yml ships (it builds each crate separately too). The
    # second invocation selects no package that owns this bin, so it leaves the
    # exe built here in place.
    _run_build_step(
        ["cmd", "/c", cargo_cmd, "build", "--release", "-p", "fauna-sync-agent"],
        "Rust build (release sync agent, alone)",
    )
    _run_build_step(
        [
            "cmd", "/c", cargo_cmd,
            "build", "--release",
            # NO --manifest-path: the apps/fauna-windows sub-workspace was unified
            # into the ROOT workspace 2026-07-22 (apps/windows.md § …; merge-gates.md
            # records the retired manifest gate), so apps/fauna-windows/Cargo.toml no
            # longer exists and passing it dies with "manifest path does not exist".
            # These crates are plain root-workspace members now.
            "-p", "fauna-nest-service",
            "-p", "fauna-bridge-service",
            "-p", "fauna-shell-ext",
            # The TerminalApp feature (TerminalApp.wxs, $(var.TuiBin)) ships the
            # terminal app beside the agent — the same binary release.yml builds.
            "-p", "fauna-tui",
        ],
        # Cold win-arm64 builds are slow: these crates (fauna-nest-service pulls in
        # the whole nest) build the full dep graph from scratch on the throwaway VM
        # this test targets. Unbounded on purpose (_run_build_step).
        "Rust build (release service crates + fauna-tui)",
    )

    # 1b. Build the Go mail-bridge MDA (cgo, gnullvm) — the Bridge feature SHIPS it.
    # Since the Track-2 wiring, ServiceBridge.wxs references $(var.MailBridgeBin) /
    # $(var.MailBridgeFfiDll) / $(var.MailBridgeLibunwindDll), so `wix build`
    # preprocesses those vars (every *.wxs is preprocessed regardless of which
    # Feature links it) and FAILS unless all three are defined. `just
    # windows-mail-bridge-build` (arm64 default, matching this arm64-only builder)
    # emits target/fauna-mail-bridge.exe + a SEPARATE gnullvm target/fauna_ffi.dll +
    # target/libunwind.dll in the ROOT target/ (installer/README.md § clean-checkout
    # step 6). Needs Go + llvm-mingw on the build box. The
    # recipe uses BARE cargo (gnullvm linker, not the MSVC link.exe the Git-Bash
    # shadow breaks), so no cargo-win.cmd wrapper here.
    # Cold: this rebuilds the gnullvm fauna-ffi + the Go cgo MDA.
    _run_build_step(
        ["just", "windows-mail-bridge-build"],
        "Go mail-bridge MDA build (just windows-mail-bridge-build)", cwd=repo,
    )

    # 1c. Regenerate the C# UniFFI bindings (FaunaApp.Core/Generated/uniffi/*.cs).
    # This MSI path publishes the WinUI app via MSBuild -t:Publish DIRECTLY, which —
    # unlike `just windows-debug`/`-release` — does NOT regenerate the bindings (the
    # csproj deliberately has no in-build regen target: it would need bash/just, only
    # on the Git-Bash PATH; see FaunaApp.Core.csproj). The Generated/ dir is gitignored,
    # so on a cold box (the throwaway VM this test targets) it's absent and the publish
    # fails with CS0246 "type could not be found" (IFfiWebClient, ProvisioningSnapshot,
    # …). `just windows-ffi-test` builds fauna-ffi (MSVC) + runs uniffi-bindgen-cs;
    # it's incremental (build-if-stale-gated) so it no-ops cheaply when already fresh.
    #
    # ⚠ The TEST flavor at the `release` profile, not the production `windows-ffi`
    # (testing.md convention 15, recipe split 2026-08-01). This test installs a
    # Release-published product and then DRIVES it through the e2e agent, so the
    # publish below sets `-p:FaunaE2eAgent=true` to compile the automation surface
    # into a Release build — and that surface calls the `*ForTest` UniFFI seams,
    # which only the test flavor's bindings export. The two must agree or the
    # publish fails to compile. This is windows' twin of a linux `--release
    # --features e2e-agent` build; the SHIPPED MSI (release.yml) sets neither.
    # Cold: builds the MSVC fauna-ffi from scratch before bindgen.
    _run_build_step(
        ["just", "windows-ffi-test", "release"],
        "windows-ffi-test release (C# UniFFI binding regen)", cwd=repo,
    )

    # 2. Publish the .NET app self-contained (skip if publish output already exists).
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
    # _max_mtime skips gitignored output (bin/, obj/, the regenerated bindings);
    # the MSVC DLL lives in the gitignored target/, so it is read on its own.
    ffi_dll = os.path.join(repo, "target", "release", "fauna_ffi.dll")
    publish_inputs_mtime = max(
        _max_mtime([
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
        _run_build_step(publish_cmd, f"WinUI self-contained publish ({tool})")

    # 3. Stage binaries
    # Root target/, not apps/fauna-windows/target/ — same 2026-07-22 unification as
    # the --manifest-path removal above; the nested target dir is never written now.
    target_dir = os.path.join(repo, "target", "release")
    for name, src_name in [
        ("fauna-sync-agent.exe", "fauna-sync-agent.exe"),
        ("fauna-nest-svc.exe", "fauna-nest-svc.exe"),
        ("fauna-bridge-svc.exe", "fauna-bridge-svc.exe"),
        # The shell-ext crate overrides [lib] name = "fauna_shell" (shell-ext/Cargo.toml),
        # so the built artifact is fauna_shell.dll — NOT the package-name default
        # fauna_shell_ext.dll. (name, src_name): stage the built fauna_shell.dll as-is.
        ("fauna_shell.dll", "fauna_shell.dll"),
        ("fauna-tui.exe", "fauna-tui.exe"),
    ]:
        src = os.path.join(target_dir, src_name)
        if not os.path.exists(src):
            pytest.fail(f"Expected build artifact missing: {src}")
        shutil.copy2(src, os.path.join(stage_dir, name))

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

    # 3b. The ShellExt feature ships Fauna-Sparse.msix as a component
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

    # 4. Build MSI
    installer_dir = os.path.join(ws_dir, "installer")
    wxs_files = glob.glob(os.path.join(installer_dir, "*.wxs"))
    wxl_files = glob.glob(os.path.join(installer_dir, "*.wxl"))
    loc_args = []
    for wxl in wxl_files:
        loc_args.extend(["-loc", wxl])
    _run_build_step(
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
            "-d", f"IconDir={os.path.join(stage_dir, 'icons')}\\",
            "-d", f"AppDir={os.path.join(stage_dir, 'app')}\\",
            "-d", f"SparsePackage={sparse_pkg}",
        ],
        # The link compresses a ~310 MB staged payload (the self-contained WinUI
        # publish) into a ~180 MB MSI: 71 s on an idle Windows host, far longer under the
        # box's ordinary multi-slot contention, which once blew a 300 s bound and
        # read as a broken installer. Unbounded like every step (_run_build_step).
        "WiX build (wix build -arch arm64)", cwd=installer_dir,
    )

    return msi_path


SOURCE_PATTERNS = [
    "apps/fauna-windows/installer/*.wxs",
    "apps/fauna-windows/*/Cargo.toml",
    "apps/fauna-windows/*/src/**/*.rs",
    "apps/fauna-windows/FaunaApp/FaunaApp/*.csproj",
    "apps/fauna-windows/FaunaApp/**/*.cs",
    "apps/fauna-windows/FaunaApp/**/*.xaml",
    "apps/fauna-windows/shell-ext/src/icons/*.ico",
    # The Go MDA the Bridge feature ships (built by `just windows-mail-bridge-build`)
    # + the gnullvm fauna_ffi.dll it links, so a change to either rebuilds the MSI.
    "bins/fauna-bridges/**/*.go",
    "libs/fauna-ffi/src/**/*.rs",
    # The terminal app the TerminalApp feature ships.
    "apps/fauna-tui/src/**/*.rs",
]


@pytest.fixture(scope="session")
def msi_path():
    """Build or return cached MSI installer."""
    repo = _get_repo_root()
    build_dir = os.path.join(repo, "build", "installer")
    msi = os.path.join(build_dir, "Fauna-Setup-arm64.msi")
    stamp = os.path.join(build_dir, "build.stamp")

    # Compute max source mtime
    full_patterns = [os.path.join(repo, p) for p in SOURCE_PATTERNS]
    current_mtime = _max_mtime(full_patterns)

    # Check cache
    if os.path.exists(msi) and os.path.exists(stamp):
        try:
            with open(stamp) as f:
                cached_mtime = float(f.read().strip())
            if cached_mtime >= current_mtime:
                return msi
        except (ValueError, OSError):
            pass

    # Cache miss — build
    result = _build_msi(repo)

    # Write stamp
    os.makedirs(build_dir, exist_ok=True)
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
    for svc in ["FaunaSync", "FaunaNest", "FaunaBridge"]:
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


def msi_uninstall(msi, remove_data=False, log_suffix=""):
    """Uninstall the MSI and clean up residual files/services."""
    cmd = ["msiexec", "/x", msi, "/qn", "/norestart"]
    if remove_data:
        cmd.append("REMOVE_USER_DATA=1")
    log_path = os.path.join(os.path.dirname(msi), f"uninstall{log_suffix}.log")
    cmd.extend(["/l*v", log_path])
    result = subprocess.run(cmd, capture_output=True)  # see _MSIEXEC_UNBOUNDED
    # Force-stop and deregister services (msiexec may leave them)
    for svc in ["FaunaSync", "FaunaNest", "FaunaBridge"]:
        subprocess.run(["sc", "stop", svc], capture_output=True)
        subprocess.run(["sc", "delete", svc], capture_output=True)
    # Remove install directory (auto-harvested files may persist)
    if os.path.isdir(INSTALL_DIR):
        shutil.rmtree(INSTALL_DIR, ignore_errors=True)
    time.sleep(2)
    return result.returncode, log_path


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


def file_installed(relative_path):
    """Check if a file exists under %ProgramFiles%\\Fauna."""
    return os.path.exists(os.path.join(INSTALL_DIR, relative_path))


def shell_dll_installed():
    """True if a version-stamped shell DLL (fauna_shell_<ver>.dll) is installed under
    %ProgramFiles%\\Fauna. Path 1 (installers/windows.md § Shell Extension) installs the
    DLL under a version-stamped @Name, so the bare fauna_shell.dll never exists on disk.
    NOTE: this test is otherwise stale pre-Step-E (it expects a FaunaSync service, and omits -ext WixToolset.Firewall.wixext) — a broader refresh is
    out of scope for the Path 1 slice; only the shell-DLL name was reconciled here."""
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

            # No services should exist
            assert not is_service_installed("FaunaSync"), \
                "FaunaSync should not be installed"
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

            # No other services
            assert not is_service_installed("FaunaSync"), \
                "FaunaSync should not be installed"
            assert not is_service_installed("FaunaBridge"), \
                "FaunaBridge should not be installed"

            # No sync, app, or shell extension
            assert not file_installed("fauna-sync-agent.exe"), \
                "fauna-sync-agent.exe should not exist"
            assert not file_installed(os.path.join("App", "FaunaApp.exe")), \
                "FaunaApp.exe should not exist"
            assert not shell_dll_installed(), \
                "shell DLL (fauna_shell_<ver>.dll) should not exist"

        finally:
            rc, log = msi_uninstall(msi_path, log_suffix="_nest")
            assert rc == 0, f"Uninstall failed (rc={rc}). See {log}"

        # Verify cleanup
        assert not is_service_installed("FaunaNest"), \
            "FaunaNest still exists after uninstall"
        assert not file_installed("fauna-nest-svc.exe"), \
            "fauna-nest-svc.exe still exists after uninstall"


class TestSyncOnly:
    """Install only the sync service — no nest, no app, no shell extension."""

    def test_install_sync_only(self, msi_path):
        rc, log = msi_install(msi_path, features=["Sync"], log_suffix="_sync")
        assert rc == 0, f"Install failed (rc={rc}). See {log}"

        try:
            # Sync service should be installed and running
            assert is_service_installed("FaunaSync"), \
                "FaunaSync not found after install"
            assert file_installed("fauna-sync-agent.exe"), \
                "fauna-sync-agent.exe missing"
            assert wait_for_service("FaunaSync"), \
                "FaunaSync not running after install"

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
        assert not is_service_installed("FaunaSync"), \
            "FaunaSync still exists after uninstall"
        assert not file_installed("fauna-sync-agent.exe"), \
            "fauna-sync-agent.exe still exists after uninstall"


class TestAllFeatures:
    """Install all features, verify everything, uninstall."""

    def test_install_all_features(self, msi_path):
        rc, log = msi_install(
            msi_path,
            features=["Sync", "ShellExt", "Nest", "Bridge", "DesktopApp"],
            log_suffix="_all",
        )
        assert rc == 0, f"Install failed (rc={rc}). See {log}"

        try:
            # All three services should be installed and running
            for svc in ["FaunaSync", "FaunaNest", "FaunaBridge"]:
                assert is_service_installed(svc), f"{svc} not found"
                assert wait_for_service(svc), f"{svc} not running"

            # All binaries should be present
            assert file_installed("fauna-sync-agent.exe"), "fauna-sync-agent.exe missing"
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
        for svc in ["FaunaSync", "FaunaNest", "FaunaBridge"]:
            assert not is_service_installed(svc), f"{svc} still exists after uninstall"
        assert not file_installed("fauna-sync-agent.exe"), \
            "fauna-sync-agent.exe still exists after uninstall"


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
INSTALLED_NEST_PORT = 7450      # fauna-nest-service SCM bind (service.rs run_as_service)
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
    services start → claim + commit storage + enable CalDAV over WS-RPC + mint a
    known-password MUA credential (the headless equivalents of the client UI's
    claim + app-password flow, via the test-only seal-helper) → the
    FaunaBridge-supervised MDA child auto-enrolls on loopback (AUTO-APPROVED because
    CalDAV is enabled — bins/fauna-nest/src/bridge_blob_handlers.rs request_enrollment
    auto-approve, BridgeRole::Mda => mail_enabled || caldav_enabled) and
    self-provisions its x25519 (register_service_user → upsert_bridge_x25519), so the
    test does **no** manual register/approve/x25519-poke — the standalone engine's
    Stage C is the SCM service's job here. The MDA fetches the self-signed floor cert
    and binds CalDAV :8443 + IMAP :993 → two Python CalDAVClients bidirectionally
    round-trip a calendar event and a stock imaplib client opens INBOX.

    Only this environment exercises the SCM virtual-account services + the supervisor
    → MDA spawn → loopback auto-enrollment path (test_caldav_imap_serving.py spawns
    the binaries directly, bypassing the SCM + supervisor). It is the
    "installed-service function" coverage installers/windows.md § Testing § Coverage
    gaps flagged as missing.

    Caveat — IMAP binds because fetch_config defaults mail_enabled to true for an
    approved bridge (`bridge_routing_handlers.rs assemble_fetch_config_reply:
    get_mail_enabled().unwrap_or(true)`), so CalDAV-only enable still serves IMAP
    (matching the proven test_caldav_imap_serving.py). If/when the Stage-5
    "default-off" flip lands (unwrap_or(false)), this test and its binaries twin must
    additionally call fauna.bridges.set_mail_enabled to serve IMAP.
    """

    def test_installed_nest_serves_caldav_and_imap(self, msi_path):
        import imaplib
        import shutil as _shutil
        import ssl

        from helpers import windows_caldav_nest as eng
        from helpers.caldav_client import CalDAVClient, build_vevent
        from helpers.caldav_roundtrip import caldav_has, wait_caldav_serving

        # Fail-fast SKIP (before the expensive install) if the test-only seal-helper
        # is not built — the headless credential mint needs it (no Fauna app UI).
        eng.seal_helper_exe()

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

        try:
            # The SCM services start on install (ServiceControl Start="install").
            assert wait_for_service("FaunaNest", timeout=30), \
                "FaunaNest service not running after install"
            assert wait_for_service("FaunaBridge", timeout=30), \
                "FaunaBridge service not running after install"

            # Claim + commit-storage + enable-CalDAV + mint a known-password MUA
            # credential against the INSTALLED nest (port 7450, %ProgramData% data
            # dir). nest_proc=None: the FaunaNest SCM service owns the process, so the
            # engine reads <data-dir>/claim-code rather than tracking a Popen.
            prov = eng.provision_caldav_on_running_nest(
                port=INSTALLED_NEST_PORT,
                nest_data_dir=_installed_nest_data_dir(),
                nest_proc=None,
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
            # Throwaway-VM teardown: REMOVE_USER_DATA=1 deletes %ProgramData%\Fauna so
            # a re-run claims fresh (the claimed nest would otherwise block re-claim).
            msi_uninstall(msi_path, remove_data=True, log_suffix="_caldav")


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

        # A new file in a synced folder is adopted and uploaded — file-sync.md
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
