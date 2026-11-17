"""The windows MSIs — their builders, their caches and the full-feature one's
staged ffi: what the collection-time prebuild and each MSI's one door must agree
on. Two packages, two doors: the full-feature MSI (`../Package.wxs`, the one
the public release builds today) through `conftest.py::msi_path`, and the nest
MSI (`installer/nest/Package.wxs`, installers/windows.md § Direct download, 1)
through `conftest.py::nest_msi_path`, read by its structure suite
`tests/platform/windows/test_nest_installer_structure.py`. The nest MSI has no
app, so its build publishes nothing and stages no ffi flavor; each door has its
own stamp and its own inputs. What follows describes the full-feature door; the
nest door is the same shape (`nest_*` below).

Two suites read the MSI through that door: the install suite
(`tests/platform/windows/test_installer.py`, elevated, since it installs it) and
the structure suite (`tests/platform/windows/test_installer_structure.py`,
unelevated, since it only queries the built database). Building needs no
elevation; only installing does. The MSI is the readers' subject, so its
publish and its WiX link stay behind the door (`build_msi`, on the reader's
declared clock); every input is a prebuild (e2e-conventions.md convention 9,
the 2026-10-30 ruling). So the prebuild has to answer, before any test runs,
the question the door answers for itself: will this run build the MSI? One
definition serves both (`msi_build_owed`), or a job would run for an MSI the
door then finds cached, or the door would miss on an input no job built.
"""

from __future__ import annotations

import glob
import os
import shutil
import subprocess
from pathlib import Path

#: The staged flavor the installer's publish bundles: the test flavor at the
#: `release` profile. The installed product is driven through the e2e agent, so
#: the publish compiles the automation surface into a Release build
#: (`-p:FaunaE2eAgent=true`), and that surface calls the `*ForTest` UniFFI seams
#: only this flavor's bindings export (testing.md convention 15). windows' twin
#: of a linux `--release --features e2e-agent` build; the SHIPPED MSI
#: (release.yml) sets neither.
RELEASE_TEST_FLAVOR = "win-arm64:release:test-helpers"

#: The MSI's own inputs — what it packages beside the staged Rust binaries. The
#: Rust those binaries are built from is `workspace_inputs`, read from the helper
#: bins' registry, never named here crate by crate.
SOURCE_PATTERNS = [
    "apps/fauna-windows/installer/*.wxs",
    # The wizard's strings and its licence page: words the MSI shows.
    "apps/fauna-windows/installer/*.wxl",
    "apps/fauna-windows/installer/*.rtf",
    # The licence files the MSI carries (Notices.wxs) and what writes its notices.
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "installer/shipped-notices.toml",
    "scripts/shipped-notices.py",
    "apps/fauna-windows/*/Cargo.toml",
    "apps/fauna-windows/*/src/**/*.rs",
    "apps/fauna-windows/FaunaApp/FaunaApp/*.csproj",
    "apps/fauna-windows/FaunaApp/**/*.cs",
    "apps/fauna-windows/FaunaApp/**/*.xaml",
    "apps/fauna-windows/shell-ext/src/icons/*.ico",
    # The Go MDA the Bridge feature ships (built by `just windows-mail-bridge-build`);
    # the gnullvm fauna_ffi.dll it links is `libs/`, a workspace input.
    "bins/fauna-bridges/**/*.go",
    # Where each staged binary's features are declared (the agent's `e2e-agent`
    # among them): a flavour change is an input like a source edit.
    "tests/e2e-unified/helpers/helper_bins.py",
]


#: The nest MSI's WiX sources, relative to `apps/fauna-windows/installer/`: its
#: own package and the fragments it shares with the full-feature one. The one
#: list both its builder (`build_nest_msi`) and its inputs (`NEST_SOURCE_PATTERNS`)
#: read; the release workflow names the same set when it builds this package.
NEST_WXS = (
    "nest/Package.wxs",
    "Directories.wxs",
    "ServiceNest.wxs",
    "ServiceBridge.wxs",
    "TerminalApp.wxs",
    "Notices.wxs",
    "UI.wxs",
)

#: The nest MSI's own inputs beside its staged Rust binaries (as
#: `SOURCE_PATTERNS` for the full-feature one).
NEST_SOURCE_PATTERNS = [
    *(f"apps/fauna-windows/installer/{wxs}" for wxs in NEST_WXS),
    "apps/fauna-windows/installer/*.wxl",
    "apps/fauna-windows/installer/*.rtf",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "installer/shipped-notices.toml",
    "scripts/shipped-notices.py",
    # The Add/Remove Programs icon (ARPPRODUCTICON), the app's own icon file.
    "apps/fauna-windows/FaunaApp/FaunaApp/Assets/AppIcon.ico",
    "bins/fauna-bridges/**/*.go",
    "tests/e2e-unified/helpers/helper_bins.py",
]


def workspace_inputs(fixture: str | None = None) -> tuple[tuple[str, ...], list[str]]:
    """The roots the staged Rust binaries are built from, and the excludes over
    them — the helper bins' own input definition (`helper_bins._newest_source_mtime`):
    the workspace every binary links (`libs/`, `Cargo.lock`, the toolchain pin)
    plus the own crates of each registry entry the MSI stages. Every staged binary — the
    agent, the nest service, the release `fauna_ffi.dll` the publish bundles —
    links most of `libs/`, so an edit to any crate there is an input. `fixture`
    names the door whose entries count (the full-feature MSI's by default)."""
    from common.nest import _EXAMPLE_SOURCE_ROOTS, _NEST_SOURCE_EXCLUDES
    from helpers import helper_bins

    door = fixture or helper_bins.WINDOWS_MSI
    roots = list(_EXAMPLE_SOURCE_ROOTS)
    for entry in helper_bins.REGISTRY.values():
        if door in entry.fixtures:
            roots.extend(rel for rel in entry.sources if rel not in roots)
    return tuple(roots), list(_NEST_SOURCE_EXCLUDES)


def is_admin() -> bool:
    """Whether this process is elevated — the install suite's skip condition, read
    from the one definition every pytest root's temp-root choice uses too. The
    MSI's build never asks it: a skip mark says which test reads the MSI."""
    from common.elevation import is_elevated

    return is_elevated()


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


def max_mtime(patterns):
    """Return the maximum mtime across all files matching the glob patterns
    (`_newest`: gitignored output never counts)."""
    return _newest([p for pattern in patterns for p in glob.glob(pattern, recursive=True)])


def _tree_files(repo: str, roots, excludes):
    """Every file under `roots` (files or directories, relative to `repo`) that
    no exclude glob matches — `build-if-stale.py`'s one exclusion semantics. A
    root the tree lacks contributes nothing."""
    from pathlib import Path

    from common.nest import _load_build_if_stale

    excluded = _load_build_if_stale()._excluded
    for rel in roots:
        root = Path(repo, rel)
        if root.is_file():
            candidates = [root]
        elif root.is_dir():
            candidates = (p for p in root.rglob("*") if p.is_file())
        else:
            continue
        for path in candidates:
            if not excluded(path, excludes):
                yield str(path)


def _newest(paths):
    """The newest mtime among `paths`.

    Gitignored files are build OUTPUT and never count as input: `bin/`/`obj/`
    (the WinUI publish writes generated *.cs there), `Generated/uniffi/` and the
    test fakes the ffi job regenerates. All of these are written AFTER `msi_path`
    takes the stamp, so counting them made its cache miss on every run following
    a build. (Tracked generated files such as `Generated/UiIds.cs` stay inputs.)
    """
    paths = list(paths)
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


def msi_paths(repo: str) -> tuple[str, str]:
    """(the MSI, its freshness stamp)."""
    build_dir = os.path.join(repo, "build", "installer")
    return (
        os.path.join(build_dir, "Fauna-Setup-arm64.msi"),
        os.path.join(build_dir, "build.stamp"),
    )


def source_mtime(repo: str) -> float:
    """The newest of the MSI's tracked inputs — what its stamp records: its own
    files (`SOURCE_PATTERNS`) and the Rust its staged binaries are built from
    (`workspace_inputs`), read in one pass so gitignored output is left out of
    both."""
    roots, excludes = workspace_inputs()
    return _newest([
        *(p for pattern in SOURCE_PATTERNS for p in glob.glob(os.path.join(repo, pattern), recursive=True)),
        *_tree_files(repo, roots, excludes),
    ])


def msi_is_cached(repo: str, current_mtime: float) -> bool:
    msi, stamp = msi_paths(repo)
    if not (os.path.exists(msi) and os.path.exists(stamp)):
        return False
    try:
        with open(stamp) as f:
            return float(f.read().strip()) >= current_mtime
    except (ValueError, OSError):
        return False


def _skip_marked(item) -> bool:
    """Whether a skip mark applies to the collected `item`: a `skip`, or a
    `skipif` with no condition or any true one. A string condition (pytest
    evaluates it against the module's globals at setup) is read as applying:
    misread the other way it owes a cold release build to a test that then
    skips, while this way a test that does run fails loudly in the build, its
    inputs never prebuilt (`HelperBinNotPrebuilt`)."""
    if any(True for _ in item.iter_markers(name="skip")):
        return True
    for mark in item.iter_markers(name="skipif"):
        conditions = (mark.kwargs["condition"],) if "condition" in mark.kwargs else mark.args
        if not conditions or any(isinstance(c, str) or bool(c) for c in conditions):
            return True
    return False


def reads_msi(item) -> bool:
    """Whether a selected item will read the MSI: it requests the door (its real
    fixture closure holds `msi_path` — the install suite's every test through
    its autouse cleanup, the structure suite's through `msi_db`) and no skip
    mark applies. Unelevated, the install suite's items stay collected with the
    door in their closure, every one skip-marked by its `pytestmark`."""
    from helpers import helper_bins
    from helpers.fixture_closure import real_fixture_closure

    return helper_bins.WINDOWS_MSI in real_fixture_closure(item) and not _skip_marked(item)


def msi_build_owed(repo: str, items) -> bool:
    """Whether a run collecting `items` will build the MSI: a selected test that
    will run reads it (`reads_msi` — the unelevated structure suite included,
    since building needs no elevation) and the stamp is stale (else the door
    returns the cached MSI, needing no input at all). With no reader the tree
    is never walked."""
    return any(reads_msi(item) for item in items) and not msi_is_cached(repo, source_mtime(repo))


# ── the nest MSI's door: the same questions, its own answers ─────────────────


def nest_msi_paths(repo: str) -> tuple[str, str]:
    """(the nest MSI, its freshness stamp) — a stamp of its own, beside the
    full-feature MSI's, so neither door's build stales the other's cache."""
    build_dir = os.path.join(repo, "build", "installer")
    return (
        os.path.join(build_dir, "Fauna-Nest-Setup-arm64.msi"),
        os.path.join(build_dir, "nest-build.stamp"),
    )


def nest_source_mtime(repo: str) -> float:
    """The newest of the nest MSI's tracked inputs (`source_mtime`'s rule over
    `NEST_SOURCE_PATTERNS` and the crates of the entries its door declares)."""
    from helpers import helper_bins

    roots, excludes = workspace_inputs(helper_bins.WINDOWS_NEST_MSI)
    return _newest([
        *(p for pattern in NEST_SOURCE_PATTERNS for p in glob.glob(os.path.join(repo, pattern), recursive=True)),
        *_tree_files(repo, roots, excludes),
    ])


def nest_msi_is_cached(repo: str, current_mtime: float) -> bool:
    msi, stamp = nest_msi_paths(repo)
    if not (os.path.exists(msi) and os.path.exists(stamp)):
        return False
    try:
        with open(stamp) as f:
            return float(f.read().strip()) >= current_mtime
    except (ValueError, OSError):
        return False


def reads_nest_msi(item) -> bool:
    """Whether a selected item will read the nest MSI (`reads_msi`'s rule, for
    its own door `nest_msi_path`)."""
    from helpers import helper_bins
    from helpers.fixture_closure import real_fixture_closure

    return helper_bins.WINDOWS_NEST_MSI in real_fixture_closure(item) and not _skip_marked(item)


def nest_msi_build_owed(repo: str, items) -> bool:
    """Whether a run collecting `items` will build the nest MSI (`msi_build_owed`'s
    rule, for its own door and stamp)."""
    return (any(reads_nest_msi(item) for item in items)
            and not nest_msi_is_cached(repo, nest_source_mtime(repo)))


def release_ffi_dll(repo: str) -> str:
    """The flavor-private dll the publish bundles a copy of, never the shared
    `target/release/fauna_ffi.dll` every host build of fauna-ffi writes
    (build-target-layout-windows.md § Cargo target dir layout (win))."""
    return os.path.join(
        repo, "target", "release", "windows-ffi", "win-arm64", "test-helpers", "fauna_ffi.dll",
    )


#: The MSI's bundled components (installer/shipped-notices.toml, the windows-msi
#: artifact's `bundled` table), as many as its notices run must list, and how
#: many licence files the build stages for them: libunwind.dll's (the
#: toolchain's own licence), the mingw-w64 runtime's the gnullvm link puts
#: into the mail bridge (its five licence files), and the WiX Toolset
#: install-time code's the package itself carries (OSMFEULA.txt, staged under
#: wix/ by `just windows-msi-notices`; its MS-RL text is a `standard_texts`
#: entry the generator prints, not a file copied). A component added to
#: the manifest changes these with it — the tier_1 pin reads both.
MSI_BUNDLED_COMPONENTS = 3
MSI_BUNDLED_FILES = 7


def msi_notices_faults(lines, rid: str = "win-arm64", artifact: str = "windows-msi") -> list[str]:
    """What the MSI's notices run (`just windows-msi-notices`) left out of the
    closure, read off the generator's verdict lines alone — the notices file is
    dependency-authored text no test opens (shipped-notices.md § How the
    notices file is produced). Three parts no library list of the restore
    names are owed in every MSI's file (§ Per channel, the Windows MSI row): the
    runtime packs the self-contained publish copied, read off that runtime's
    deps.json; the .NET app host FaunaApp.exe is, for that runtime; and the
    bundled components — the Bridge feature's libunwind.dll, the mingw-w64
    runtime linked into the mail bridge and the WiX Toolset code the package
    itself carries — with every licence file their build staged copied.
    `artifact` names the manifest artifact the run wrote: the nest MSI's
    (`windows-nest-msi`) ships no app, so only its bundled line and its verdict
    are asked of it. Empty = all present."""
    import re

    lines = list(lines)
    faults = []
    if artifact == "windows-msi":
        packs = [m for m in (re.match(rf"shipped-notices: nuget runtime packs from FaunaApp\.deps\.json \(\S*/{re.escape(rid)}\): (\d+)$", ln) for ln in lines) if m]
        if not packs or int(packs[-1].group(1)) == 0:
            faults.append(f"no runtime pack read off the {rid} publish's FaunaApp.deps.json: the .NET runtime is missing from the notices")
        if not any(re.match(rf"shipped-notices: nuget app host Microsoft\.NETCore\.App\.Host\.{re.escape(rid)}@\S+ \(", ln) for ln in lines):
            faults.append(f"the .NET app host for {rid} (FaunaApp.exe is its launcher) is not in the notices")
    bundled = (rf"shipped-notices: {re.escape(artifact)} bundled: components={MSI_BUNDLED_COMPONENTS} "
               rf"files-copied={MSI_BUNDLED_FILES} fallbacks=0 left-out=0 refused=0$")
    if not any(re.match(bundled, ln) for ln in lines):
        faults.append("libunwind.dll, the mingw-w64 runtime and the WiX Toolset install-time code are not in the notices "
                      "with every licence file their build staged copied")
    if not any(re.match(rf"shipped-notices: {re.escape(artifact)} OK ", ln) for ln in lines):
        faults.append(f"the generator printed no OK verdict for {artifact}")
    return faults


def require_release_test_flavor(repo: str) -> None:
    """Refuse a publish of any other staged flavor. The WinUI project consumes one
    fixed pair of paths whatever the flavor, so a wrong one compiles, packages
    and installs without a word: the dev dll in a Release app, or bindings the
    e2e agent cannot compile against."""
    marker = os.path.join(repo, "apps", "fauna-windows", ".ffi-flavor")
    try:
        with open(marker, encoding="utf-8") as f:
            staged = f.read().strip()
    except OSError as exc:
        staged = f"unreadable ({exc})"
    if staged != RELEASE_TEST_FLAVOR:
        raise RuntimeError(
            f"the staged ffi flavor is {staged!r}, not {RELEASE_TEST_FLAVOR!r} "
            f"({marker}): the collection-time `just windows-ffi-test release` job "
            "stages it last, so a build or a recipe that ran after it restaged "
            "another flavor. Publishing now would package the wrong core."
        )


# WinUI XAML must be published with MSBuild, NOT `dotnet build/publish`: on win-arm64
# `dotnet` invokes the XAML compiler out-of-process under x86 emulation and it crashes
# (conftest._build_windows_app uses the same path for the Debug build). Absent (e.g.
# an x64 box, where there is no emulation), the publish falls back to `dotnet publish`.
_MSBUILD = r"C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\MSBuild\Current\Bin\MSBuild.exe"


def build_msi(repo: str) -> str:
    """Build the MSI from source, the readers' subject: the publish, the
    licence files and the WiX link, nothing else — every input it stages is a
    collection-time prebuild (`msi_build_owed`), replayed here and never built.
    Returns the MSI path. Called by the door alone (`conftest.py::msi_path`),
    on a cache miss; needs no elevation."""
    import pytest

    from common.nest import _build_slot_cmd
    from helpers.build_step import run_build_step
    from helpers.helper_bins import helper_bin

    build_dir = os.path.join(repo, "build", "installer")
    stage_dir = os.path.join(build_dir, "stage", "arm64")
    msi_path = os.path.join(build_dir, "Fauna-Setup-arm64.msi")

    os.makedirs(stage_dir, exist_ok=True)
    os.makedirs(os.path.join(stage_dir, "icons"), exist_ok=True)
    os.makedirs(os.path.join(stage_dir, "app"), exist_ok=True)

    ws_dir = os.path.join(repo, "apps", "fauna-windows")

    # 0. Ensure WiX extensions are installed (version must match WiX v6)
    _ensure_wix_extensions()

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
        # A compile, so it takes a `build` grant like every other (re-entered
        # when the caller already holds one), and the grant sizes MSBuild's
        # `-m` to its cores.
        if os.path.exists(_MSBUILD):
            publish_cmd = _build_slot_cmd(Path(repo)) + [
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
            publish_cmd = _build_slot_cmd(Path(repo)) + [
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

    # 3. Build MSI — in the release's shape: `-d NoSparsePackage=1`, as
    # release.yml's MSI step passes it. The sparse identity package retired with its
    # sources (installers/windows.md § Direct download, 6), so nothing can build the
    # `Fauna-Sparse.msix` ShellExt.wxs's FaunaSparsePackage component would ship; the
    # define leaves the component out, explicitly (a build that merely forgets
    # $(var.SparsePackage) still dies WIX0150). The register CA then finds no package
    # beside the shell DLL and does nothing, as on a released install.
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
            "-d", "NoSparsePackage=1",
        ],
        # The link compresses a ~310 MB staged payload (the self-contained WinUI
        # publish) into a ~180 MB MSI: 71 s on an idle Windows host, far longer under the
        # box's ordinary multi-slot contention, which once blew a 300 s bound and
        # read as a broken installer. Unbounded like every step (run_build_step).
        "WiX build (wix build -arch arm64)", cwd=installer_dir,
    )

    return msi_path


def _ensure_wix_extensions() -> None:
    """The three WiX extensions every MSI build loads, at the WiX tool's own
    version — both builders (`build_msi`, `build_nest_msi`)."""
    import pytest

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


def build_nest_msi(repo: str) -> str:
    """Build the nest MSI from source (installers/windows.md § Direct download, 1):
    the licence files and the WiX link of `NEST_WXS`, nothing else — no publish,
    since it carries no app, and every staged binary a collection-time prebuild
    (`nest_msi_build_owed`), replayed here and never built. Returns the MSI path.
    Called by its door alone (`conftest.py::nest_msi_path`), on a cache miss;
    needs no elevation."""
    import pytest

    from helpers.build_step import run_build_step
    from helpers.helper_bins import helper_bin

    build_dir = os.path.join(repo, "build", "installer")
    stage_dir = os.path.join(build_dir, "stage-nest", "arm64")
    msi_path, _stamp = nest_msi_paths(repo)
    if os.path.isdir(stage_dir):
        shutil.rmtree(stage_dir)
    os.makedirs(os.path.join(stage_dir, "icons"))
    ws_dir = os.path.join(repo, "apps", "fauna-windows")

    _ensure_wix_extensions()

    # The four Rust binaries the Nest, Bridge and Terminal App features ship.
    for name in ["fauna-nest-svc", "fauna-bridge-svc", "fauna-tui", "fauna-cli"]:
        src = helper_bin(name)
        shutil.copy2(src, os.path.join(stage_dir, src.name))

    # The Go MDA and its gnullvm runtime pieces, from the root target/ (as build_msi).
    root_target = os.path.join(repo, "target")
    for fname in ["fauna-mail-bridge.exe", "fauna_ffi.dll", "libunwind.dll"]:
        src = os.path.join(root_target, fname)
        if not os.path.exists(src):
            pytest.fail(
                f"Expected MDA build artifact missing: {src} "
                "(just windows-mail-bridge-build should have staged it in target/)"
            )
        shutil.copy2(src, os.path.join(stage_dir, fname))

    # The Add/Remove Programs icon (ARPPRODUCTICON) — the package's one icon; it
    # installs no overlay icons, having no shell extension.
    app_icon = os.path.join(ws_dir, "FaunaApp", "FaunaApp", "Assets", "AppIcon.ico")
    if not os.path.exists(app_icon):
        pytest.fail(f"App icon missing: {app_icon} — nest/Package.wxs stages it as ARPPRODUCTICON.")
    shutil.copy2(app_icon, os.path.join(stage_dir, "icons", "AppIcon.ico"))

    # The three licence files, for the nest MSI's own manifest artifact (no app,
    # so no publish runs before it); a refusing generator fails the build here.
    notices_dir = os.path.join(stage_dir, "notices")
    notices_out = run_build_step(
        ["just", "windows-nest-msi-notices", notices_dir.replace("\\", "/"), "arm64",
         os.path.join(root_target, "fauna-mail-bridge.exe").replace("\\", "/")],
        "the nest MSI's licence and notices files (just windows-nest-msi-notices)", cwd=repo,
    )
    faults = msi_notices_faults(notices_out, "win-arm64", "windows-nest-msi")
    if faults:
        pytest.fail("the nest MSI's notices leave out part of its closure:\n  " + "\n  ".join(faults)
                    + "\nthe generator's verdict lines:\n" + "\n".join(l for l in notices_out if l.startswith("shipped-notices")))

    installer_dir = os.path.join(ws_dir, "installer")
    wxs_files = [os.path.join(installer_dir, *rel.split("/")) for rel in NEST_WXS]
    loc_args = []
    for wxl in glob.glob(os.path.join(installer_dir, "*.wxl")):
        loc_args.extend(["-loc", wxl])
    run_build_step(
        [
            "wix", "build", "-arch", "arm64",
            "-ext", "WixToolset.Util.wixext",
            "-ext", "WixToolset.UI.wixext",
            "-ext", "WixToolset.Firewall.wixext",
            "-o", msi_path,
        ] + wxs_files + loc_args + [
            "-d", f"NestBin={os.path.join(stage_dir, 'fauna-nest-svc.exe')}",
            "-d", f"BridgeBin={os.path.join(stage_dir, 'fauna-bridge-svc.exe')}",
            "-d", f"MailBridgeBin={os.path.join(stage_dir, 'fauna-mail-bridge.exe')}",
            "-d", f"MailBridgeFfiDll={os.path.join(stage_dir, 'fauna_ffi.dll')}",
            "-d", f"MailBridgeLibunwindDll={os.path.join(stage_dir, 'libunwind.dll')}",
            "-d", f"TuiBin={os.path.join(stage_dir, 'fauna-tui.exe')}",
            "-d", f"CliBin={os.path.join(stage_dir, 'fauna-cli.exe')}",
            "-d", f"IconDir={os.path.join(stage_dir, 'icons')}\\",
            "-d", f"NoticesDir={notices_dir}\\",
        ],
        "WiX build of the nest MSI (wix build -arch arm64)", cwd=installer_dir,
    )
    return msi_path
