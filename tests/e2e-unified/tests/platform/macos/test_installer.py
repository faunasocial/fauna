"""E2E tests: macOS .pkg installer — dry-run validation and full install lifecycle.

The all-in-one `.pkg` (installers/macos.md § Distribution Channels) installs four
selectable components: the desktop app (social.fauna.app → /Applications, default-ON,
non-relocatable) plus the three services. The server services (nest, bridge) install
as machine-service `LaunchDaemon`s under dedicated hidden service users (`_fauna` /
`_fauna-bridge`), system data dir `/Library/Application Support/Fauna`; sync stays a
per-user `LaunchAgent` (§ launchd jobs, decision 2026-06-24). TestDryRun asserts the
postinstall scripts encode that shape + the app component's /Applications install
(no sudo, CI-safe); TestFullInstall proves it on a real install (sudo)."""

import glob
import json
import os
import pwd
import stat
import plistlib
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

from helpers.root_build_guard import (
    refuse_root_build,
    root_droppings_message,
    root_owned_paths,
)


def _is_root():
    if sys.platform != "darwin":
        return False
    return os.getuid() == 0


pytestmark = [pytest.mark.skipif(sys.platform != "darwin", reason="macOS-only"), pytest.mark.tier_3]

#: The app component's own minimum macOS — its compile target and its bundle's
#: LSMinimumSystemVersion (installers/macos.md § Implementation status today).
APP_MIN_OS = "15.0"
#: The Distribution-XML predicate that gates the app choice on it.
APP_FLOOR_PREDICATE = "appComponentSupported()"
SYSTEM_DATA_DIR = "/Library/Application Support/Fauna"
#: Where the daemons' launchd redirects used to point. Nothing writes here any
#: more: each daemon keeps its own size-capped `fauna_log` file under its data
#: dir, and a launchd redirect is a second copy launchd never rotates
#: (observability.md § Persistence & privacy).
RETIRED_SYSTEM_LOG_DIR = "/Library/Logs/Fauna"

BINARIES = [
    # The social.fauna.nest LaunchDaemon runs fauna-nest-daemon (the macOS nest
    # service shell with launchd socket activation + the shared serve loop), NOT
    # the standalone fauna-nest.
    "fauna-nest-daemon",
    # The sync component's LaunchAgent runs fauna-sync-agent (sync-agent.md
    # § Packaging + lifecycle); the legacy daemon is gone (§ Headless deployment).
    "fauna-sync-agent",
    # The social.fauna.bridge LaunchDaemon runs fauna-bridge-supervisor (the macOS MDA
    # supervisor), which spawns the Go fauna-mail-bridge MDA child. The pre-I6
    # fauna-bridge-imap/fauna-bridge-daemon pair was removed.
    "fauna-bridge-supervisor",
    "fauna-mail-bridge",
    # The terminal app — the .pkg's fifth component (social.fauna.tui), the
    # same binary the per-OS release archive carries (installers/tui.md § The
    # ratified channel). Beside fauna-sync-agent, which it resolves as a sibling.
    "fauna-tui",
    "fauna-uninstall",
]

# Shared libraries installed beside the binaries in /usr/local/bin. The Go MDA is
# a cgo binary that resolves libfauna_ffi.dylib from beside itself via an
# @loader_path rpath (build.sh Step 3a). Not an executable, but a Mach-O arm64
# artifact the bridge component ships and fauna-uninstall removes.
DYLIBS = [
    "libfauna_ffi.dylib",
]

# The .pkg metapackage's five selectable components (distribution.xml choices). The
# desktop app (social.fauna.app) installs Fauna.app to /Applications; the three service
# components install to /usr/local (binaries + LaunchDaemons/Agent); the terminal app
# (social.fauna.tui, default selected like Windows' TerminalApp feature) installs
# fauna-tui to /usr/local/bin with no scripts.
CHOICE_IDS = [
    "social.fauna.app",
    "social.fauna.nest",
    "social.fauna.sync",
    "social.fauna.tui",
    "social.fauna.bridge",
]

# The desktop app component installs the whole Fauna.app bundle to /Applications
# (machine-wide, BundleIsRelocatable=false), mirroring the Windows DesktopApp feature
# (installers/macos.md § Distribution Channels). Unlike the service components it
# stages a .app bundle, not flat /usr/local/bin binaries.
APP_COMPONENT_PKG = "fauna-app.pkg"
APP_INSTALL_PATH = "/Applications/Fauna.app"

# Server services → machine LaunchDaemons (/Library/LaunchDaemons/) under service
# users. Sync → a per-user LaunchAgent (~/Library/LaunchAgents/) running
# fauna-sync-agent (A4 cutover). No retired label is swept any more — the
# compat-remnant sweep removed that (installers/macos.md § Identifier domain;
# `test_apple_identifier_pins.py::test_no_installer_script_sweeps_a_retired_label`).
DAEMON_LABELS = ["social.fauna.nest", "social.fauna.bridge"]
AGENT_LABELS = ["social.fauna.sync-agent"]

# Dedicated hidden service users the postinstall creates via dscl (the macOS twin
# of Linux-native's `useradd -r fauna` / `fauna-bridge`).
SERVICE_USERS = ["_fauna", "_fauna-bridge"]

COMPONENT_BINARIES = {
    "fauna-nest.pkg": ["fauna-nest-daemon", "fauna-uninstall"],
    "fauna-sync.pkg": ["fauna-sync-agent", "fauna-uninstall"],
    "fauna-bridge.pkg": [
        "fauna-bridge-supervisor",
        "fauna-mail-bridge",
        "libfauna_ffi.dylib",
        "fauna-uninstall",
    ],
    "fauna-tui.pkg": ["fauna-tui", "fauna-uninstall"],
}

# Components that ship NO Scripts archive by design: the terminal app only drops
# `fauna-tui` into /usr/local/bin — no service to bootstrap, no user to create — so
# `pkgbuild` runs without `--scripts` (installers/tui.md § The ratified channel).
SCRIPTLESS_COMPONENTS = {"fauna-tui.pkg"}

SOURCE_PATTERNS = [
    "installer/macos/**/*",
    "bins/fauna-nest/src/**/*.rs",
    "bins/fauna-nest-daemon/src/**/*.rs",
    "bins/fauna-sync-agent/src/**/*.rs",
    "bins/fauna-bridge-supervisor/src/**/*.rs",
    "libs/fauna-mda-supervisor/src/**/*.rs",
    "bins/fauna-bridges/**/*.go",
    "libs/*/src/**/*.rs",
    "Cargo.toml",
    "apps/fauna-apple/Fauna-macOS/Resources/Info.plist",
    "justfile",
]
# The desktop app component (social.fauna.app) is built from the Swift app sources
# via `just mac-app` (justfile), so a change there must rebuild the cached .pkg.
# Those sources are watched via _tracked_swift_mtime() rather than a glob in
# SOURCE_PATTERNS — see that function for why a glob can't be used here.


def _get_repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    )
    return os.path.normpath(result.stdout.strip())


def _max_mtime(patterns):
    best = 0.0
    for pattern in patterns:
        for path in glob.glob(pattern, recursive=True):
            try:
                best = max(best, os.path.getmtime(path))
            except OSError:
                pass
    return best


def _tracked_swift_mtime(repo):
    """Max mtime among git-TRACKED *.swift under apps/fauna-apple — the hand-edited
    macOS/iOS app + FaunaKit sources that `just mac-app` builds Fauna.app from.

    Watching git-tracked files (not a glob) deliberately excludes every build
    OUTPUT that lives in that tree, all of which are gitignored: the FFI Swift
    bindings (apps/fauna-apple/generated/ and apps/fauna-apple/FaunaFFI.xcframework/),
    the generated UniFFI bindings in FaunaFFISwift/Sources/ (which sit beside the
    one hand-written shim, FFICompat.swift), and the SwiftPM .build/ dependency
    cache. Those outputs are regenerated by `just mac-app` *during
    the build the .pkg fixture triggers*, so counting them would make every build
    self-invalidate the next cache check — the cache would never hit after a build.
    The Rust the FFI bindings derive from is already watched via
    "libs/*/src/**/*.rs", so dropping the generated Swift loses no real signal.
    """
    result = subprocess.run(
        ["git", "-C", repo, "ls-files", "-z", "--", "apps/fauna-apple"],
        capture_output=True, text=True,
    )
    best = 0.0
    for rel in result.stdout.split("\0"):
        if not rel.endswith(".swift"):
            continue
        try:
            best = max(best, os.path.getmtime(os.path.join(repo, rel)))
        except OSError:
            pass
    return best


def _get_console_user():
    """Get the actual console user (not root) for the per-user sync agent paths."""
    if _is_root():
        result = subprocess.run(
            ["scutil"],
            input="show State:/Users/ConsoleUser\n",
            capture_output=True, text=True,
        )
        for line in result.stdout.splitlines():
            if "Name :" in line:
                user = line.split(":")[-1].strip()
                if user and user != "loginwindow":
                    return user
        # Fallback: SUDO_USER is set when running under sudo
        return os.environ.get("SUDO_USER", "")
    return os.environ.get("USER", "")


def _get_user_home(username):
    result = subprocess.run(
        ["dscl", ".", "-read", f"/Users/{username}", "NFSHomeDirectory"],
        capture_output=True, text=True,
    )
    if result.returncode != 0 or not result.stdout.strip():
        pytest.fail(f"Could not resolve home directory for {username}: {result.stderr}")
    return result.stdout.split()[-1]


def _build_pkg(repo):
    """Build the unsigned .pkg installer via `just pkg-unsigned`. Returns the
    .pkg path.

    Routed through `_build_via_just` (conftest.py) rather than a bare
    `subprocess.run(..., timeout=900)` — that gave this a hard 900s ceiling
    duplicating pytest-timeout's own per-test bound, so a `just pkg-unsigned`
    that queued for a machine-wide build slot past 900s (routine under fleet
    contention) failed
    this exact call even when nothing was broken. `_build_via_just` has no
    fixed ceiling (bounded only by `build-slot.py`'s own `FAUNA_SLOT_TIMEOUT`),
    streams output, retries once on a genuine break, and reaps orphans if the
    harness reaps this process mid-build. Combined with moving the call
    outside every per-test budget (`_ensure_pkg_built` below), this closes the
    "bound inversion" class `_prebuild_binaries`'s docstring documents for
    every other binary this suite builds — `pkg_path` was the one builder
    that had never been hoisted into it.
    """
    from conftest import _build_via_just

    _build_via_just("pkg-unsigned", "building the unsigned .pkg installer")

    # Find the built .pkg
    version = "0.1.0"
    with open(os.path.join(repo, "Cargo.toml")) as f:
        for line in f:
            if line.startswith("version"):
                version = line.split('"')[1]
                break
    pkg = os.path.join(repo, "build", f"Fauna-{version}.pkg")
    if not os.path.exists(pkg):
        raise RuntimeError(f"Expected .pkg not found at {pkg}")
    return pkg


def _ensure_pkg_built() -> str:
    """Build the .pkg (or reuse a fresh cached one) — memoized per pytest
    process via `_memoized_build`, exactly like every other binary this suite
    builds (conftest.py's "Binary ensure layer").

    Called from TWO doors, same as every sibling: `_prebuild_binaries`
    (conftest.py's `_ensure_macos_pkg_built`) at COLLECTION time — outside
    every per-test pytest-timeout budget — and the `pkg_path` fixture below,
    which on a warm run just replays the memo. Raises plain exceptions, never
    `pytest.fail` — `_pytest.outcomes.Failed` is a `BaseException`, not an
    `Exception`, so it would escape `_memoized_build`'s and
    `_prebuild_binaries`'s `except Exception` handling and crash collection
    outright instead of being caught, printed, and replayed like every other
    builder's failure.
    """
    from conftest import _memoized_build

    def build():
        repo = _get_repo_root()
        build_dir = os.path.join(repo, "build", "pkg")
        stamp = os.path.join(build_dir, "build.stamp")

        # Find existing .pkg
        existing = glob.glob(os.path.join(repo, "build", "Fauna-*.pkg"))

        # Compute max source mtime — glob patterns plus the git-tracked Swift app
        # sources (which can't be globbed without sweeping in build outputs).
        full_patterns = [os.path.join(repo, p) for p in SOURCE_PATTERNS]
        current_mtime = max(_max_mtime(full_patterns), _tracked_swift_mtime(repo))

        # Check cache
        cache_is_fresh = False
        if existing and os.path.exists(stamp):
            try:
                with open(stamp) as f:
                    cached_mtime = float(f.read().strip())
                cache_is_fresh = cached_mtime >= current_mtime
            except (ValueError, OSError):
                pass

        # Refuse to build under root — `build.sh` runs `just mac-app release` (cargo
        # + xcodebuild), which would leave root-owned artifacts in the cargo target
        # dir, ~/.cache/fauna-apple-ffi, and apps/fauna-apple/.build, breaking every
        # later non-root `just` build. The full-install legs need sudo, so the
        # intended flow is: build the .pkg as your normal user first (the non-sudo
        # TestDryRun leg does this), then `sudo pytest`. A stale cache under root
        # means that didn't happen. The decision itself lives in
        # `helpers/root_build_guard.py` so it can be proved without a real sudo run
        # (`tests/test_root_build_guard.py`) — it shipped 2026-06-28 with no test and
        # ~3.8G of root-owned artifacts accumulated anyway.
        if refuse_root_build(is_root=_is_root(), cache_is_fresh=cache_is_fresh):
            raise RuntimeError(
                "Installer .pkg missing or stale while running as root. Building it "
                "here would create root-owned cargo/Swift artifacts that break later "
                "non-root `just` builds. Build it as your normal user first, e.g.\n"
                "    installer/macos/build.sh\n"
                "(or run the non-sudo TestDryRun leg once), then re-run with sudo."
            )

        if cache_is_fresh:
            return existing[0]

        # Cache miss (non-root) — build
        result = _build_pkg(repo)

        # Write stamp
        os.makedirs(build_dir, exist_ok=True)
        with open(stamp, "w") as f:
            f.write(str(current_mtime))

        return result

    return _memoized_build("pkg-installer", build)


@pytest.fixture(scope="session")
def pkg_path():
    """Build or return the cached .pkg installer.

    Normally a memo replay: `_prebuild_binaries`
    (tests/e2e-unified/conftest.py, via `_ensure_macos_pkg_built`) already
    built it at collection time, outside every per-test timeout budget — see
    `_ensure_pkg_built`'s docstring above for why that placement is a
    correctness requirement, not a speed-up.
    """
    return _ensure_pkg_built()


def _extract_component_scripts(pkg_path, tmp_path, component):
    """Return {script_name: text} for a component's Scripts archive (postinstall +
    common.sh). Sudo-free — used by TestDryRun to assert the postinstall encodes
    the machine-service shape."""
    subprocess.run(
        ["xar", "-xf", pkg_path],
        cwd=str(tmp_path), capture_output=True, check=True,
    )
    scripts_archive = tmp_path / component / "Scripts"
    assert scripts_archive.exists(), f"No Scripts archive in {component}"
    scripts_dir = tmp_path / f"{component}.scripts"
    scripts_dir.mkdir(exist_ok=True)
    subprocess.run(
        ["cpio", "-idm"],
        input=subprocess.run(
            ["gzip", "-dc"],
            input=scripts_archive.read_bytes(),
            capture_output=True, check=True,
        ).stdout,
        capture_output=True, cwd=str(scripts_dir),
    )
    out = {}
    for f in scripts_dir.iterdir():
        if f.is_file():
            out[f.name] = f.read_text()
    return out


def _run_uninstall(username):
    """Run fauna-uninstall if the binary exists."""
    uninstall = "/usr/local/bin/fauna-uninstall"
    if os.path.exists(uninstall):
        subprocess.run(
            [uninstall, username],
            capture_output=True, timeout=30,
        )
        time.sleep(1)


def _install_pkg(pkg_path, components=None):
    """Install the .pkg via the CLI `installer`, force-SELECTING the given component
    choices (default: all three).

    The nest/bridge SERVER components default UNSELECTED (distribution.xml
    `start_selected="false"` — the 2026-06-26 default-OFF design matching Windows
    "Nest Service default OFF"), and the command-line `installer(8)` honors those
    `selected` defaults exactly as the GUI does. So a full-install test must
    force-select via `-applyChoiceChangesXML` — the headless twin of ticking the
    components in the Installer GUI — or the install would skip nest+bridge and put
    down only sync. Returns the CompletedProcess."""
    if components is None:
        components = CHOICE_IDS
    choice_dicts = "\n".join(
        "  <dict>\n"
        "    <key>choiceIdentifier</key><string>{cid}</string>\n"
        "    <key>choiceAttribute</key><string>selected</string>\n"
        "    <key>attributeSetting</key><integer>1</integer>\n"
        "  </dict>".format(cid=cid)
        for cid in components
    )
    xml = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
        '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
        '<plist version="1.0">\n<array>\n' + choice_dicts + '\n</array>\n</plist>\n'
    )
    with tempfile.NamedTemporaryFile(
        "w", suffix=".xml", delete=False,
    ) as f:
        f.write(xml)
        choices_path = f.name
    try:
        return subprocess.run(
            ["installer", "-applyChoiceChangesXML", choices_path,
             "-pkg", pkg_path, "-target", "/", "-dumplog"],
            capture_output=True, text=True, timeout=120,
        )
    finally:
        os.remove(choices_path)


@pytest.fixture(scope="session", autouse=True)
def _no_root_owned_build_artifacts():
    """Fail loudly if this checkout holds root-owned build artifacts.

    The regression-proof for the refusal above. The refusal covers the build
    path we know about; this checks the OUTCOME, so a root build reaching the
    checkout by some path nobody anticipated still gets caught — which is the
    whole difference between a guard and a second copy of the same assumption.

    Runs only under sudo, because only a root run can create the condition, so
    the ordinary non-root inner loop pays nothing for it.

    Checked BEFORE the run as well as after: the 2026-08-16 droppings survived
    seven weeks precisely because nothing ever looked, and a fifteen-minute
    install run should not be spent before saying the checkout was already
    dirty. A failure at the START means leftovers from an earlier run (remove
    them); a failure at the END means something built as root during THIS run
    (that is the bug).
    """
    if not _is_root():
        yield
        return
    repo = _get_repo_root()
    before = root_owned_paths(repo)
    if before:
        pytest.fail(root_droppings_message(repo, before, when="BEFORE the run started"))
    yield
    after = root_owned_paths(repo)
    if after:
        pytest.fail(root_droppings_message(repo, after, when="AFTER the run — created by it"))


@pytest.fixture(scope="session", autouse=True)
def _cleanup_stale_install(pkg_path):
    """Uninstall any leftover Fauna installation before and after tests."""
    username = _get_console_user()
    if username:
        _run_uninstall(username)
    yield
    # Post-test safety net: clean up even if test_uninstall failed
    if username and os.path.exists("/usr/local/bin/fauna-nest-daemon"):
        _run_uninstall(username)
    if _is_root():
        # Manual cleanup if fauna-uninstall already self-deleted.
        for b in BINARIES + DYLIBS:
            path = f"/usr/local/bin/{b}"
            if os.path.exists(path):
                os.remove(path)
        if os.path.isdir(APP_INSTALL_PATH):
            subprocess.run(["rm", "-rf", APP_INSTALL_PATH], capture_output=True)
        for label in DAEMON_LABELS:
            subprocess.run(["launchctl", "bootout", f"system/{label}"], capture_output=True)
            plist = f"/Library/LaunchDaemons/{label}.plist"
            if os.path.exists(plist):
                os.remove(plist)
        if username:
            user_home = _get_user_home(username)
            uid = subprocess.run(
                ["id", "-u", username], capture_output=True, text=True,
            ).stdout.strip()
            for label in AGENT_LABELS:
                subprocess.run(
                    ["launchctl", "bootout", f"gui/{uid}/{label}"],
                    capture_output=True,
                )
                plist = os.path.join(user_home, "Library", "LaunchAgents", f"{label}.plist")
                if os.path.exists(plist):
                    os.remove(plist)


class TestDryRun:
    """Validate the built .pkg without installing — no sudo required."""

    def test_pkg_is_valid_archive(self, pkg_path):
        """The .pkg is a valid xar archive with expected contents."""
        result = subprocess.run(
            ["xar", "-tf", pkg_path],
            capture_output=True, text=True,
        )
        assert result.returncode == 0, f"xar -tf failed: {result.stderr}"
        entries = result.stdout.strip().splitlines()
        assert "Distribution" in entries, f"No Distribution in archive: {entries}"
        for component in COMPONENT_BINARIES:
            assert component in entries, f"{component} not in archive: {entries}"
        assert APP_COMPONENT_PKG in entries, \
            f"{APP_COMPONENT_PKG} (desktop app component) not in archive: {entries}"

    def test_bundled_agent_carries_its_own_entitlements(self, pkg_path):
        """The `Fauna.app` the .pkg ships — the sibling `build/Release/Fauna.app`
        it was assembled from — is signed INSIDE-OUT: the bundled
        `fauna-sync-agent` (the copy the sync postinstall's LaunchAgent prefers)
        carries the AGENT's entitlements — no `com.apple.security.application-groups`
        at all, since its state lives in the user domain and it never opens the
        TCC-protected container (installers/macos.md § Identifier domain, item 6)
        — each embedded `.appex` carries ITS OWN, and the app carries BOTH its
        groups (the shared File Provider group and the app-only account keychain
        group).

        The appex arm rides this test rather than a parallel one because it is the
        SAME defect in a second nested bundle, and the same one line of
        `sign-app-bundle.sh` decides both. It matters more for the extension: the
        File Provider appex is sandboxed and names ONE group (its replica
        container), while the app names two — the account keychain group exists
        precisely so that a sandboxed extension can never reach the identity seed
        (installers/macos.md § Identifier domain). A `--deep` app signature would
        hand it that group.

        Why an artifact-level pin on top of the entitlements-FILE pin
        (`test_apple_identifier_pins.py`): `codesign --deep --entitlements`
        re-stamps every nested Mach-O with the OUTER bundle's entitlements, so the
        agent's file said "no group" while the shipped bundled copy carried both of
        the app's — measured 2026-08-25 on the first signed build after the drop.
        Only the built artifact can witness what the signing pipeline
        (`installer/macos/sign-app-bundle.sh`) actually produced; ad-hoc
        (`pkg-unsigned`, this fixture) and Developer ID builds go through the same
        script, so the pin holds for both."""
        app = Path(pkg_path).parent / "Release" / "Fauna.app"
        assert app.is_dir(), f"the staged app bundle is missing beside the .pkg: {app}"

        def entitlements(path):
            out = subprocess.run(
                ["codesign", "-d", "--entitlements", "-", "--xml", str(path)],
                capture_output=True, check=True,
            ).stdout
            start = out.find(b"<?xml")
            return plistlib.loads(out[start:]) if start >= 0 else {}

        agent = entitlements(app / "Contents" / "MacOS" / "fauna-sync-agent")
        assert "com.apple.security.application-groups" not in agent, (
            f"the bundled fauna-sync-agent carries app groups "
            f"{agent['com.apple.security.application-groups']!r} — the app's "
            f"entitlements were stamped onto it (a --deep signature); the agent "
            f"must carry only its own file, which names no group"
        )
        assert agent.get("com.apple.security.network.client") is True, agent

        bundle = entitlements(app)
        assert bundle.get("com.apple.security.application-groups") == [
            "7457N3M72H.group.social.fauna.shared",
            "7457N3M72H.group.social.fauna.account",
        ], bundle

        # The extensions. Their PRESENCE is the other half of what this pins: the
        # shipped bundle carried no `Contents/PlugIns/` at all until 2026-08-28
        # (`just mac-app` assembled it from SwiftPM, which cannot build an app
        # extension), so System Settings → Extensions listed nothing to enable and
        # no Fauna location could appear in Finder — installers/macos.md
        # § App extensions.
        plugins = app / "Contents" / "PlugIns"
        assert plugins.is_dir(), (
            f"{plugins} is missing — the shipped app embeds no extension, so no "
            f"Fauna location can appear in Finder (installers/macos.md § App "
            f"extensions)"
        )
        appexes = sorted(p.name for p in plugins.glob("*.appex"))
        assert appexes == [
            "Fauna-FileProvider.appex", "Fauna-FileProviderUI.appex", "Fauna-Widget.appex",
        ], appexes

        fp = entitlements(plugins / "Fauna-FileProvider.appex")
        assert fp.get("com.apple.security.application-groups") == [
            "7457N3M72H.group.social.fauna.shared",
        ], (
            f"the File Provider appex carries app groups "
            f"{fp.get('com.apple.security.application-groups')!r} — it must name "
            f"ONLY the shared group (its replica container). The account keychain "
            f"group is the app's alone, so the sandboxed extension can never read "
            f"the identity seed; seeing it here means the app's entitlements were "
            f"stamped onto the appex (a --deep signature)"
        )
        assert fp.get("com.apple.security.app-sandbox") is True, fp
        assert fp.get("com.apple.security.network.client") is True, fp

        # The UI appex names NO group and no network — the strongest witness that
        # each nested bundle got its own file rather than the app's.
        fpui = entitlements(plugins / "Fauna-FileProviderUI.appex")
        assert "com.apple.security.application-groups" not in fpui, fpui
        assert "com.apple.security.network.client" not in fpui, fpui
        assert fpui.get("com.apple.security.app-sandbox") is True, fpui

        # The home-screen widget reads one snapshot file from the shared container
        # and nothing else: the shared group, sandboxed, and NO network — it never
        # connects anywhere (apps/common.md § Home-screen widget).
        widget = entitlements(plugins / "Fauna-Widget.appex")
        assert widget.get("com.apple.security.application-groups") == [
            "7457N3M72H.group.social.fauna.shared",
        ], widget
        assert widget.get("com.apple.security.app-sandbox") is True, widget
        assert "com.apple.security.network.client" not in widget, widget

        # …and the inside-out signature seals cleanly as a whole.
        subprocess.run(
            ["codesign", "--verify", "--deep", "--strict", str(app)],
            capture_output=True, check=True,
        )

    def test_distribution_xml_structure(self, pkg_path, tmp_path):
        """Distribution XML has correct structure and no unresolved placeholders."""
        # Extract Distribution from the xar archive
        subprocess.run(
            ["xar", "-xf", pkg_path, "Distribution"],
            cwd=str(tmp_path), capture_output=True, check=True,
        )
        dist_file = tmp_path / "Distribution"
        assert dist_file.exists(), "Distribution not extracted"

        tree = ET.parse(dist_file)
        root = tree.getroot()

        # Check choices
        choices = root.findall(".//choice")
        choice_ids = [c.get("id") for c in choices]
        assert sorted(choice_ids) == sorted(CHOICE_IDS), \
            f"Expected choices {CHOICE_IDS}, got {choice_ids}"

        # Default state (2026-06-26, macos.md § launchd jobs → Default state): the
        # nest + bridge SERVER components default UNSELECTED (the desktop is primarily
        # a client; selecting a server component is the install-time opt-in to
        # self-host, matching Windows "Nest Service default OFF"). Sync stays selected.
        choice_by_id = {c.get("id"): c for c in choices}
        assert choice_by_id["social.fauna.nest"].get("start_selected") == "false", \
            "nest component must default UNSELECTED (start_selected=\"false\")"
        assert choice_by_id["social.fauna.bridge"].get("start_selected") == "false", \
            "bridge component must default UNSELECTED (start_selected=\"false\")"
        assert choice_by_id["social.fauna.sync"].get("start_selected") != "false", \
            "sync component must stay selected by default"
        # The desktop app defaults SELECTED (default-ON, mirroring the Windows
        # DesktopApp feature) — the common macOS case is "just the client" (app +
        # sync). installers/macos.md § Distribution Channels / § launchd jobs.
        assert choice_by_id["social.fauna.app"].get("start_selected") != "false", \
            "app component must default SELECTED (the desktop app is default-ON)"

        # ...but only on a box that can RUN it. The floors are per-component by
        # design (installers/macos.md § Implementation status today — the floors
        # note): services 13.0, app 15.0. The volume-check above stays at 13.0 so a
        # headless services-only install still reaches an older Mac mini, which
        # means the APP choice is what has to carry its own floor — otherwise a
        # macOS 13 box installs an app whose LSMinimumSystemVersion is 15.0 and
        # Finder refuses to open it, a successful install of a broken product.
        app_choice = choice_by_id["social.fauna.app"]
        for attr in ("start_enabled", "start_selected"):
            value = app_choice.get(attr) or ""
            assert APP_FLOOR_PREDICATE in value, (
                f"the app choice's {attr} must be gated on {APP_FLOOR_PREDICATE!r} so "
                f"macOS < {APP_MIN_OS} cannot select an app it cannot launch; got {value!r}"
            )
        script = root.find(".//{*}script") if root.find(".//{*}script") is not None \
            else root.find(".//script")
        assert script is not None and script.text, \
            "distribution.xml declares no <script>, so the app-floor predicate is undefined"
        assert APP_FLOOR_PREDICATE.rstrip("()") in script.text, \
            f"{APP_FLOOR_PREDICATE} is referenced by a choice but never defined"
        assert APP_MIN_OS in script.text, \
            f"the app-floor predicate must compare against {APP_MIN_OS}: {script.text!r}"

        # Check os-version
        os_versions = root.findall(".//os-version")
        assert any(v.get("min") == "13.0" for v in os_versions), \
            "Missing os-version min=13.0"

        # Check architecture
        options = root.findall(".//options")
        assert any("arm64" in (o.get("hostArchitectures", "") ) for o in options), \
            "Missing hostArchitectures=arm64"

        # Check no unresolved placeholders
        xml_text = dist_file.read_text()
        assert "__VERSION__" not in xml_text, "Unresolved __VERSION__ placeholder"
        assert "__SIZE_" not in xml_text, "Unresolved __SIZE_ placeholder"

    def test_component_packages_contain_binaries(self, pkg_path, tmp_path):
        """Each component .pkg contains the expected binary payloads."""
        # Extract all component packages from the xar
        subprocess.run(
            ["xar", "-xf", pkg_path],
            cwd=str(tmp_path), capture_output=True, check=True,
        )

        for component, expected_bins in COMPONENT_BINARIES.items():
            component_pkg = tmp_path / component
            assert component_pkg.exists(), f"{component} not extracted"

            # Inside a metapackage, component pkgs are expanded directories
            # with a gzip'd cpio Payload archive. List files from it.
            payload = component_pkg / "Payload"
            assert payload.exists(), \
                f"No Payload in {component}"
            decompressed = subprocess.run(
                ["gzip", "-dc"],
                input=payload.read_bytes(),
                capture_output=True, check=True,
            )
            result = subprocess.run(
                ["cpio", "-it"],
                input=decompressed.stdout,
                capture_output=True,
            )
            payload_names = [
                os.path.basename(f)
                for f in result.stdout.decode().strip().splitlines()
            ]

            for binary in expected_bins:
                assert binary in payload_names, \
                    f"{binary} not found in {component} payload: {payload_names}"

    def test_mda_dylib_relocated_in_payload(self, pkg_path, tmp_path):
        """The Go MDA staged in the bridge .pkg payload resolves libfauna_ffi.dylib
        via @rpath/@loader_path, not the absolute build path (build.sh Step 3a).

        Headless proof of the relocation in the actual built package — the
        full-install twin (TestFullInstall.test_mda_dylib_linkage_relocated)
        re-checks it on the installed binary under sudo."""
        subprocess.run(
            ["xar", "-xf", pkg_path],
            cwd=str(tmp_path), capture_output=True, check=True,
        )
        payload = tmp_path / "fauna-bridge.pkg" / "Payload"
        assert payload.exists(), "No Payload in fauna-bridge.pkg"
        # Decompress + extract the MDA binary from the gzip'd cpio payload.
        extract_dir = tmp_path / "bridge-payload"
        extract_dir.mkdir()
        decompressed = subprocess.run(
            ["gzip", "-dc"], input=payload.read_bytes(),
            capture_output=True, check=True,
        )
        subprocess.run(
            ["cpio", "-idm"], input=decompressed.stdout,
            capture_output=True, cwd=str(extract_dir),
        )
        mda = extract_dir / "fauna-mail-bridge"
        assert mda.exists(), f"fauna-mail-bridge not in payload: {list(extract_dir.iterdir())}"

        linkage = subprocess.run(
            ["otool", "-L", str(mda)], capture_output=True, text=True,
        ).stdout
        ffi_lines = [l for l in linkage.splitlines() if "libfauna_ffi.dylib" in l]
        assert ffi_lines, f"no libfauna_ffi.dylib reference: {linkage}"
        assert all("@rpath/libfauna_ffi.dylib" in l for l in ffi_lines), \
            f"MDA dylib reference not relocated to @rpath: {ffi_lines}"
        assert "/target/" not in linkage, \
            f"MDA still references an absolute build path: {linkage}"
        rpaths = subprocess.run(
            ["otool", "-l", str(mda)], capture_output=True, text=True,
        ).stdout
        assert "@loader_path" in rpaths, \
            f"MDA missing the @loader_path rpath: {rpaths}"

    def test_app_component_installs_to_applications(self, pkg_path, tmp_path):
        """The desktop app component (fauna-app.pkg) installs Fauna.app to
        /Applications, non-relocatable. Its PackageInfo must declare
        install-location="/Applications", relocatable="false", and a Fauna.app
        bundle — so the bundle always lands in /Applications (the component plist
        pins BundleIsRelocatable=false, build.sh Step 5a) rather than relocating
        onto a prior copy found elsewhere on disk. Mirrors the Windows DesktopApp
        feature (installers/macos.md § Distribution Channels)."""
        subprocess.run(
            ["xar", "-xf", pkg_path],
            cwd=str(tmp_path), capture_output=True, check=True,
        )
        component = tmp_path / APP_COMPONENT_PKG
        assert component.exists(), f"{APP_COMPONENT_PKG} not extracted"
        pkginfo = component / "PackageInfo"
        assert pkginfo.exists(), f"No PackageInfo in {APP_COMPONENT_PKG}"
        root = ET.parse(pkginfo).getroot()
        assert root.get("install-location") == "/Applications", \
            f"app install-location {root.get('install-location')!r} != /Applications"
        assert root.get("relocatable") == "false", \
            "app component must be non-relocatable (BundleIsRelocatable=false)"
        assert root.get("identifier") == "social.fauna.app", \
            f"app component identifier {root.get('identifier')!r} != social.fauna.app"
        # The payload bundles are the direct <bundle path=...> children (the nested
        # <bundle id=.../> under <upgrade-bundle> etc. carry no path).
        bundle_paths = [b.get("path") for b in root.findall("bundle")]
        assert any(p and p.endswith("Fauna.app") for p in bundle_paths), \
            f"app component must ship Fauna.app: {bundle_paths}"

    def test_postinstall_scripts_are_executable(self, pkg_path, tmp_path):
        """Each component package has an executable postinstall script — except
        the scriptless components (SCRIPTLESS_COMPONENTS), which must carry none."""
        subprocess.run(
            ["xar", "-xf", pkg_path],
            cwd=str(tmp_path), capture_output=True, check=True,
        )

        for component in COMPONENT_BINARIES:
            component_pkg = tmp_path / component
            # Inside a metapackage, Scripts is a gzip'd cpio archive.
            # Extract it to inspect the postinstall script.
            scripts_archive = component_pkg / "Scripts"
            if component in SCRIPTLESS_COMPONENTS:
                assert not scripts_archive.exists(), \
                    f"{component} is declared scriptless but carries a Scripts archive"
                continue
            assert scripts_archive.exists(), \
                f"No Scripts archive in {component}"
            scripts_dir = tmp_path / f"{component}.scripts"
            scripts_dir.mkdir()
            subprocess.run(
                ["cpio", "-idm"],
                input=subprocess.run(
                    ["gzip", "-dc"],
                    input=scripts_archive.read_bytes(),
                    capture_output=True, check=True,
                ).stdout,
                capture_output=True, cwd=str(scripts_dir),
            )
            postinstall = scripts_dir / "postinstall"
            assert postinstall.exists(), \
                f"No postinstall script in {component}"
            assert os.access(str(postinstall), os.X_OK), \
                f"postinstall in {component} is not executable"

    def test_nest_postinstall_encodes_launchdaemon(self, pkg_path, tmp_path):
        """The nest component's postinstall (+ common.sh) encodes the machine-service
        LaunchDaemon shape: a hidden `_fauna` service user via dscl, a plist in
        /Library/LaunchDaemons running fauna-nest-daemon under UserName _fauna with
        FAUNA_DATA_DIR + the FaunaNest socket-activation dict for :443, bootstrapped
        into the SYSTEM domain (not gui/$UID), enabled + auto-started when selected
        (no Disabled — the opt-in is the .pkg component selection, 2026-06-26)."""
        scripts = _extract_component_scripts(pkg_path, tmp_path, "fauna-nest.pkg")
        post = scripts.get("postinstall", "")
        common = scripts.get("common.sh", "")
        combined = post + "\n" + common

        # The LaunchDaemon binary is fauna-nest-daemon, NOT the standalone fauna-nest.
        assert "/usr/local/bin/fauna-nest-daemon" in post, \
            "nest plist must run fauna-nest-daemon"
        # The daemon builds its own config from FAUNA_DATA_DIR — its ProgramArguments
        # must carry no `--config` arg (the plist's XML arg form, not the comment).
        assert "<string>--config</string>" not in post, \
            "the nest plist ProgramArguments must not pass --config"
        # Service user + system data dir wiring.
        assert "ensure_service_user _fauna" in post
        assert "dscl" in common and "IsHidden" in common, \
            "common.sh must create a hidden service user via dscl"
        assert "<key>UserName</key>" in post and "_fauna" in post
        assert "FAUNA_DATA_DIR" in post
        assert SYSTEM_DATA_DIR in (post + common)
        # The socket-activation dict (must match launch_activate_socket("FaunaNest")).
        assert "<key>Sockets</key>" in post
        assert "FaunaNest" in post
        assert "443" in post
        # System-domain LaunchDaemon, not a per-user agent.
        assert "/Library/LaunchDaemons" in common
        assert "launchctl bootstrap system" in common
        assert "gui/" not in post, "the nest daemon must not bootstrap into gui/$UID"
        # Enabled-when-selected (2026-06-26, macos.md § launchd jobs → Default state):
        # a SELECTED nest component installs the daemon ENABLED (no Disabled key), so
        # `launchctl bootstrap system` + RunAtLoad start it right after install — the
        # macOS twin of the Windows MSI auto-starting a ticked service. The opt-in is
        # the .pkg component selection (default-unselected), not a post-install enable.
        assert "<key>Disabled</key>" not in post, \
            "a selected nest component must install the daemon ENABLED (no Disabled key)"
        # The daemon logs to its own size-capped file under FAUNA_DATA_DIR/logs;
        # a launchd redirect is a second, never-rotated copy of every line.
        assert "StandardOutPath" not in post and "StandardErrorPath" not in post, \
            "the nest LaunchDaemon must not redirect stdout/stderr into a file"
        # Substring match: catches the per-user ~/Library/Logs/Fauna too, which
        # nothing the .pkg installs writes either (the agent logs under its
        # Application Support sync/logs/).
        assert RETIRED_SYSTEM_LOG_DIR not in combined, \
            f"nothing may create or write {RETIRED_SYSTEM_LOG_DIR} (system or per-user) any more"
        # An upgrade whose plist changed (this very redirect removal is one)
        # must rewrite + reload it — kickstart alone keeps the old plist forever.
        assert 'launchctl bootout "system/$label"' in common, \
            "install_launchdaemon must reload a changed plist on upgrade"

    def test_bridge_postinstall_encodes_launchdaemon(self, pkg_path, tmp_path):
        """The bridge component's postinstall encodes a `_fauna-bridge` machine
        LaunchDaemon running fauna-bridge-supervisor."""
        scripts = _extract_component_scripts(pkg_path, tmp_path, "fauna-bridge.pkg")
        post = scripts.get("postinstall", "")
        # The supervisor logs (itself + the captured MDA child) to its own
        # size-capped file under bridge/logs; no launchd redirect.
        assert "StandardOutPath" not in post and "StandardErrorPath" not in post, \
            "the bridge LaunchDaemon must not redirect stdout/stderr into a file"
        assert RETIRED_SYSTEM_LOG_DIR not in post and "FAUNA_SYSTEM_LOG_DIR" not in post, \
            f"nothing may create or write {RETIRED_SYSTEM_LOG_DIR} any more"
        assert "ensure_service_user _fauna-bridge" in post
        assert "/usr/local/bin/fauna-bridge-supervisor" in post
        assert "<key>UserName</key>" in post and "_fauna-bridge" in post
        assert "FAUNA_DATA_DIR" in post
        # Enabled-when-selected (2026-06-26): a selected bridge component installs the
        # supervisor daemon ENABLED (no Disabled); it then self-gates the Go MDA child
        # on the nest's caldav/imap flags. macos.md § launchd jobs → Default state.
        assert "<key>Disabled</key>" not in post, \
            "a selected bridge component must install the daemon ENABLED (no Disabled key)"

    def test_sync_postinstall_stays_per_user(self, pkg_path, tmp_path):
        """Sync stays a per-user LaunchAgent: ~/Library/LaunchAgents + gui/$UID,
        NOT a /Library/LaunchDaemons system daemon. Since the A4 cutover the
        agent is fauna-sync-agent, ENABLED (no Disabled key, no `run`
        subcommand — flags only)."""
        scripts = _extract_component_scripts(pkg_path, tmp_path, "fauna-sync.pkg")
        post = scripts.get("postinstall", "")
        assert "install_launchagent" in post, "sync must use the per-user agent path"
        assert "resolve_console_user" in post
        assert "/usr/local/bin/fauna-sync-agent" in post, \
            "the LaunchAgent must run fauna-sync-agent (A4 cutover; bare " \
            "fallback for a services-only install)"
        assert "/Applications/Fauna.app/Contents/MacOS/fauna-sync-agent" in post, \
            "the LaunchAgent must PREFER the bundled agent copy — bundle " \
            "attribution puts its TCC prompt on the app's own subject " \
            "(installers/macos.md § Identifier domain, measured 2026-08-23)"
        assert "<string>run</string>" not in post, \
            "fauna-sync-agent takes no `run` subcommand (flags only)"
        assert "<key>Disabled</key>" not in post, \
            "the sync agent ships ENABLED (an unprovisioned agent idles harmlessly)"
        assert 'install_launchagent "social.fauna.sync-agent"' in post, \
            "the LaunchAgent label must be social.fauna.sync-agent"
        assert "install_launchdaemon" not in post, \
            "sync must NOT install a system LaunchDaemon"
        assert "_fauna-bridge" not in post and "ensure_service_user" not in post
        # The agent writes its own size-capped log (`fauna_log`, under its data
        # dir); a launchd redirect is a second, NEVER-rotated copy of every line —
        # one reached 6.6 GB (observability.md § Persistence & privacy).
        assert "StandardOutPath" not in post and "StandardErrorPath" not in post, \
            "the sync agent's LaunchAgent must not redirect stdout/stderr to a file " \
            "launchd never rotates"

    def test_welcome_html_present(self, pkg_path, tmp_path):
        """The welcome.html resource is included in the metapackage."""
        subprocess.run(
            ["xar", "-xf", pkg_path],
            cwd=str(tmp_path), capture_output=True, check=True,
        )
        resources = tmp_path / "Resources"
        # welcome.html may be at Resources/welcome.html or inside the archive
        # The metapackage includes it via --resources
        welcome_found = False
        if resources.exists():
            for f in resources.rglob("welcome.html"):
                welcome_found = True
                break
        # Also check the xar listing directly
        if not welcome_found:
            result = subprocess.run(
                ["xar", "-tf", pkg_path],
                capture_output=True, text=True,
            )
            entries = result.stdout.strip().splitlines()
            welcome_found = any("welcome.html" in e for e in entries)

        assert welcome_found, "welcome.html not found in the metapackage"


@pytest.mark.skipif(not _is_root(), reason="Requires sudo")
class TestFullInstall:
    """Full install/uninstall lifecycle — requires sudo."""

    def test_install_succeeds(self, pkg_path):
        # Force-select all three components — nest+bridge default UNSELECTED, so a
        # plain `installer -pkg` would install only sync (see _install_pkg).
        result = _install_pkg(pkg_path)
        assert result.returncode == 0, \
            f"installer failed (rc={result.returncode}):\n" \
            f"STDOUT:\n{result.stdout}\nSTDERR:\n{result.stderr}"

    def test_binaries_installed(self, pkg_path):
        for binary in BINARIES + DYLIBS:
            path = f"/usr/local/bin/{binary}"
            assert os.path.exists(path), f"{binary} not found at {path}"

    def test_app_installed_to_applications(self, pkg_path):
        """The desktop app component installs Fauna.app to /Applications as a valid
        bundle with an executable. The install force-selects all components, incl.
        social.fauna.app (which is default-selected anyway)."""
        assert os.path.isdir(APP_INSTALL_PATH), \
            f"Fauna.app not installed at {APP_INSTALL_PATH}"
        exe = os.path.join(APP_INSTALL_PATH, "Contents", "MacOS", "Fauna")
        assert os.path.exists(exe), f"app executable missing at {exe}"
        assert os.access(exe, os.X_OK), f"{exe} is not executable"
        # The bundle ships the sync agent beside the app executable — the .dmg
        # channel's agent source (the spawner's first resolution path; `just
        # mac-app` bundles it, sync-agent.md § Packaging + lifecycle).
        agent = os.path.join(APP_INSTALL_PATH, "Contents", "MacOS", "fauna-sync-agent")
        assert os.path.exists(agent), f"bundled sync agent missing at {agent}"
        assert os.access(agent, os.X_OK), f"{agent} is not executable"

    def test_installed_app_registers_its_file_provider_appex(self, pkg_path):
        """Installing the .pkg makes macOS DISCOVER the File Provider extension.

        The mechanism nobody was testing. Row 7 put `Fauna-FileProvider.appex` in the
        shipped bundle and asserted it on the built artifact; whether macOS then
        *registers* the extension on install was left to a human squinting at System
        Settings -- and the first supervised round (2026-08-28) showed why that is a
        bad witness: the pane lists nothing for Fauna until a domain is registered, so
        an install-then-look runbook reports "missing" for a correctly installed,
        correctly registered extension. `pluginkit` answers the real question directly
        and needs no human, no domain, and no sign-in.

        Deliberately NOT asserted here: the enable state (a per-user decision this
        install does not make -- pluginkit reports no explicit state until someone
        toggles it) and enumeration (blocked on the -34018 keychain gate). Those are the last inch; discovery is not.
        """
        appex = os.path.join(
            APP_INSTALL_PATH, "Contents", "PlugIns", "Fauna-FileProvider.appex")
        assert os.path.isdir(appex), f"FP appex missing from installed bundle at {appex}"

        listed = subprocess.run(
            ["pluginkit", "-m", "-v", "-p", "com.apple.fileprovider-nonui"],
            capture_output=True, text=True, timeout=60,
        ).stdout
        ours = [l for l in listed.splitlines()
                if "social.fauna.fauna.FileProvider" in l]
        assert ours, (
            "macOS did not register the File Provider extension on install: "
            f"social.fauna.fauna.FileProvider absent from the fileprovider-nonui "
            f"plug-in list.\n{listed}")
        # ...and registered from the INSTALLED bundle, not a build-tree twin. A twin
        # winning this resolution is what fails every NSFileProviderManager.register
        # with FP -2001/-2014 (installers/macos.md § App extensions, twin gotcha).
        assert any(APP_INSTALL_PATH in l for l in ours), (
            "the FP extension is registered from somewhere other than "
            f"{APP_INSTALL_PATH} -- a same-bundle-id twin won LaunchServices "
            f"resolution:\n" + "\n".join(ours))

    def test_binaries_are_executable(self, pkg_path):
        # fauna-uninstall is a shell script, not a compiled binary
        compiled_binaries = [b for b in BINARIES if b != "fauna-uninstall"]
        for binary in BINARIES:
            path = f"/usr/local/bin/{binary}"
            assert os.access(path, os.X_OK), f"{path} is not executable"
        # Both the compiled executables and the FFI dylib must be Mach-O arm64.
        for binary in compiled_binaries + DYLIBS:
            path = f"/usr/local/bin/{binary}"
            result = subprocess.run(
                ["file", path],
                capture_output=True, text=True,
            )
            assert "Mach-O" in result.stdout, \
                f"{binary} is not a Mach-O binary: {result.stdout}"
            assert "arm64" in result.stdout, \
                f"{binary} is not arm64: {result.stdout}"

    def test_mda_dylib_linkage_relocated(self, pkg_path):
        """The installed Go MDA resolves libfauna_ffi.dylib via @rpath/@loader_path,
        not the absolute build path (build.sh Step 3a relocation)."""
        mda = "/usr/local/bin/fauna-mail-bridge"
        linkage = subprocess.run(
            ["otool", "-L", mda], capture_output=True, text=True,
        ).stdout
        ffi_lines = [l for l in linkage.splitlines() if "libfauna_ffi.dylib" in l]
        assert ffi_lines, f"no libfauna_ffi.dylib reference in {mda}: {linkage}"
        assert all("@rpath/libfauna_ffi.dylib" in l for l in ffi_lines), \
            f"MDA dylib reference not relocated to @rpath: {ffi_lines}"
        assert "/target/" not in linkage, \
            f"MDA still references an absolute build path: {linkage}"
        rpaths = subprocess.run(
            ["otool", "-l", mda], capture_output=True, text=True,
        ).stdout
        assert "@loader_path" in rpaths, \
            f"MDA missing the @loader_path rpath: {rpaths}"

    def test_service_users_created(self, pkg_path):
        """The postinstall creates the hidden _fauna / _fauna-bridge service users
        via dscl (no login shell)."""
        for name in SERVICE_USERS:
            result = subprocess.run(
                ["dscl", ".", "-read", f"/Users/{name}"],
                capture_output=True, text=True,
            )
            assert result.returncode == 0, f"service user {name} not created: {result.stderr}"
            shell = subprocess.run(
                ["dscl", ".", "-read", f"/Users/{name}", "UserShell"],
                capture_output=True, text=True,
            ).stdout
            assert "/usr/bin/false" in shell or "/sbin/nologin" in shell, \
                f"{name} has a login shell: {shell}"

    def test_launchdaemons_created(self, pkg_path):
        """nest/bridge install as /Library/LaunchDaemons; sync as a per-user agent."""
        for label in DAEMON_LABELS:
            plist = f"/Library/LaunchDaemons/{label}.plist"
            assert os.path.exists(plist), f"{label}.plist not found at {plist}"
        username = _get_console_user()
        user_home = _get_user_home(username)
        for label in AGENT_LABELS:
            plist = os.path.join(user_home, "Library", "LaunchAgents", f"{label}.plist")
            assert os.path.exists(plist), f"{label}.plist not found at {plist}"

    def test_daemon_plist_structure(self, pkg_path):
        """nest/bridge LaunchDaemon plists: UserName, FAUNA_DATA_DIR, opt-in; the
        nest plist also has the FaunaNest socket-activation dict for :443."""
        expected = {
            "social.fauna.nest": {
                "binary": "/usr/local/bin/fauna-nest-daemon",
                "user": "_fauna",
                "has_socket": True,
            },
            "social.fauna.bridge": {
                "binary": "/usr/local/bin/fauna-bridge-supervisor",
                "user": "_fauna-bridge",
                "has_socket": False,
            },
        }
        for label, expect in expected.items():
            plist_path = f"/Library/LaunchDaemons/{label}.plist"
            result = subprocess.run(
                ["plutil", "-convert", "json", "-o", "-", plist_path],
                capture_output=True, text=True,
            )
            assert result.returncode == 0, f"plutil failed for {label}: {result.stderr}"
            plist = json.loads(result.stdout)

            assert plist["Label"] == label
            assert plist.get("UserName") == expect["user"], \
                f"{label}: UserName {plist.get('UserName')} != {expect['user']}"
            assert plist["ProgramArguments"][0] == expect["binary"]
            assert plist["KeepAlive"] is True, f"{label}: KeepAlive not true"
            # Enabled-when-selected (2026-06-26, macos.md § launchd jobs → Default
            # state): a SELECTED nest/bridge component installs the daemon ENABLED —
            # no `Disabled` key — so `launchctl bootstrap system` + RunAtLoad start
            # it right after install. The opt-in is the .pkg component selection
            # (default-unselected), not a post-install enable. Matches the shipped
            # postinstall + the dry-run test_nest_postinstall_encodes_launchdaemon.
            assert "Disabled" not in plist, \
                f"{label}: a selected component must install the daemon ENABLED " \
                f"(no Disabled key)"
            assert plist.get("RunAtLoad") is True, f"{label}: RunAtLoad not true"
            env = plist.get("EnvironmentVariables", {})
            assert env.get("FAUNA_DATA_DIR") == SYSTEM_DATA_DIR, \
                f"{label}: FAUNA_DATA_DIR != {SYSTEM_DATA_DIR}"
            # Each daemon logs to its own size-capped fauna_log file; a launchd
            # redirect is never rotated (observability.md § Persistence & privacy).
            assert "StandardOutPath" not in plist and "StandardErrorPath" not in plist, \
                f"{label}: the daemon plist must not redirect stdout/stderr"
            if expect["has_socket"]:
                sockets = plist.get("Sockets", {})
                assert "FaunaNest" in sockets, f"{label}: missing FaunaNest socket"
                svc = sockets["FaunaNest"].get("SockServiceName")
                assert str(svc) == "443", f"{label}: FaunaNest SockServiceName {svc} != 443"

    def test_sync_agent_plist_structure(self, pkg_path):
        """The per-user LaunchAgent runs fauna-sync-agent as the user (no UserName
        key), ENABLED (RunAtLoad + KeepAlive, no Disabled), with no `run`
        subcommand."""
        username = _get_console_user()
        user_home = _get_user_home(username)
        plist_path = os.path.join(
            user_home, "Library", "LaunchAgents", "social.fauna.sync-agent.plist",
        )
        result = subprocess.run(
            ["plutil", "-convert", "json", "-o", "-", plist_path],
            capture_output=True, text=True,
        )
        assert result.returncode == 0, f"plutil failed for sync-agent: {result.stderr}"
        plist = json.loads(result.stdout)
        assert plist["Label"] == "social.fauna.sync-agent"
        # Mirrors the postinstall's own preference: the bundled copy when the
        # app component landed (bundle attribution — one app-worded TCC prompt),
        # the bare binary on a services-only install.
        bundled = "/Applications/Fauna.app/Contents/MacOS/fauna-sync-agent"
        expected_exec = bundled if os.path.exists(bundled) \
            else "/usr/local/bin/fauna-sync-agent"
        assert plist["ProgramArguments"][0] == expected_exec
        assert "run" not in plist["ProgramArguments"], \
            "fauna-sync-agent takes no `run` subcommand"
        assert "UserName" not in plist, "the per-user sync agent must not set UserName"
        assert "Disabled" not in plist, \
            "the sync agent ships ENABLED (an unprovisioned agent idles harmlessly)"
        assert plist.get("RunAtLoad") is True
        assert plist.get("KeepAlive") is True
        assert "StandardOutPath" not in plist and "StandardErrorPath" not in plist, \
            "the sync agent logs to its own size-capped file; a launchd redirect " \
            "is never rotated"

    def test_sync_agent_resolves_reachable_production_paths(self, pkg_path):
        """The installed sync agent — launchd's own environment, no --data-dir —
        must reach ready and bind its REAL production socket + data root, not
        merely have a well-formed plist. `install_launchagent` (common.sh)
        bootstraps/kickstarts it right after this class's install, so it should
        already be up; poll rather than assume immediacy.

        Closes the harness blind spot the tier_3 `agent_process_tier3.rs` suite
        cannot: that harness always passes `--data-dir` + an isolated `HOME`
        (testing.md § point 10), so it never drives the launchd-spawned process
        this asserts against — the exact gap that hid the 2026-07-20 `.pkg`
        install crash-loop."""
        username = _get_console_user()
        user_home = _get_user_home(username)
        uid = pwd.getpwnam(username).pw_uid
        label = "social.fauna.sync-agent"

        deadline = time.time() + 20
        printed = None
        while time.time() < deadline:
            printed = subprocess.run(
                ["launchctl", "print", f"gui/{uid}/{label}"],
                capture_output=True, text=True,
            )
            if printed.returncode == 0:
                break
            time.sleep(1)
        assert printed is not None and printed.returncode == 0, \
            f"{label} not loaded in the console user's GUI domain: " \
            f"{printed.stderr if printed else 'launchctl never ran'}"

        # The REAL resolved socket path (SyncPaths::production_base_dir's twin,
        # unix_transport::default_socket_path) — not a --data-dir-scoped stand-in.
        socket_path = os.path.join(
            user_home, "Library", "Application Support", "Fauna", "sync-agent.sock",
        )
        deadline = time.time() + 20
        while time.time() < deadline and not os.path.exists(socket_path):
            time.sleep(1)
        assert os.path.exists(socket_path), \
            f"agent socket never appeared at its real production path {socket_path}"
        assert stat.S_ISSOCK(os.stat(socket_path).st_mode), \
            f"{socket_path} exists but is not a unix socket"

        # The REAL resolved data root (SyncPaths::production_base_dir) — the
        # user-domain `…/Fauna/sync` beside the socket since 2026-08-25, when the
        # agent's state left the TCC-protected app-group container (a launchd
        # agent is prompted there on every instance — installers/macos.md
        # § Identifier domain, item 5). The container is now the File Provider
        # extension's root only; the agent must never create anything under it.
        data_root = os.path.join(
            user_home, "Library", "Application Support", "Fauna", "sync",
        )
        assert os.path.isdir(data_root), \
            f"agent data root never resolved/created at {data_root}"
        # (Deliberately no "nothing under the container" assertion here: a box's
        # existing container state stays untouched, and a
        # sudo'd test process probing the container is itself a TCC subject.
        # The never-the-container pin is tier_1, in `SyncPaths`'s own tests.)

    def test_system_directories_created(self, pkg_path):
        """The system data dir is owned by _fauna, its bridge/ subdir by _fauna-bridge."""
        assert os.path.isdir(SYSTEM_DATA_DIR), f"{SYSTEM_DATA_DIR} does not exist"
        fauna_uid = pwd.getpwnam("_fauna").pw_uid
        assert os.stat(SYSTEM_DATA_DIR).st_uid == fauna_uid, \
            f"{SYSTEM_DATA_DIR} not owned by _fauna"
        bridge_dir = os.path.join(SYSTEM_DATA_DIR, "bridge")
        if os.path.isdir(bridge_dir):
            bridge_uid = pwd.getpwnam("_fauna-bridge").pw_uid
            assert os.stat(bridge_dir).st_uid == bridge_uid, \
                f"{bridge_dir} not owned by _fauna-bridge"

    def test_service_load_unload(self, pkg_path):
        """The nest LaunchDaemon is loaded in the SYSTEM domain after a selected
        install (enabled-when-selected → install bootstraps it), and unload + reload
        via the modern launchctl API both succeed. (Uses bootout/bootstrap, not the
        legacy load/unload, which is unreliable on an already-bootstrapped job.)"""
        label = "social.fauna.nest"
        plist_path = f"/Library/LaunchDaemons/{label}.plist"
        # Enabled-when-selected: the install bootstrapped it into the system domain.
        printed = subprocess.run(
            ["launchctl", "print", f"system/{label}"],
            capture_output=True, text=True,
        )
        assert printed.returncode == 0, \
            f"{label} not loaded in the system domain after install: {printed.stderr}"
        # Unload, then reload from the plist — both must succeed.
        booted_out = subprocess.run(
            ["launchctl", "bootout", f"system/{label}"],
            capture_output=True, text=True,
        )
        assert booted_out.returncode == 0, \
            f"launchctl bootout failed for {label}: {booted_out.stderr}"
        bootstrapped = subprocess.run(
            ["launchctl", "bootstrap", "system", plist_path],
            capture_output=True, text=True,
        )
        assert bootstrapped.returncode == 0, \
            f"launchctl bootstrap failed for {label}: {bootstrapped.stderr}"

    def test_upgrade_preserves_plists(self, pkg_path):
        username = _get_console_user()
        user_home = _get_user_home(username)

        # Record plist mtimes before upgrade (system daemons + per-user sync).
        plist_paths = {
            label: f"/Library/LaunchDaemons/{label}.plist" for label in DAEMON_LABELS
        }
        for label in AGENT_LABELS:
            plist_paths[label] = os.path.join(
                user_home, "Library", "LaunchAgents", f"{label}.plist",
            )
        mtimes_before = {
            label: os.path.getmtime(p) for label, p in plist_paths.items() if os.path.exists(p)
        }
        assert len(mtimes_before) == len(plist_paths), \
            f"Not all plists exist before upgrade: {list(mtimes_before.keys())}"

        # Install again (upgrade) — force-select the same components as the first
        # install, else the upgrade would skip the default-unselected nest+bridge.
        result = _install_pkg(pkg_path)
        assert result.returncode == 0, \
            f"Upgrade install failed:\nSTDOUT:\n{result.stdout}\nSTDERR:\n{result.stderr}"

        # Plists should NOT be overwritten (install helpers skip if exists).
        for label, p in plist_paths.items():
            assert os.path.exists(p), f"{label}.plist missing after upgrade"
            assert os.path.getmtime(p) == mtimes_before[label], \
                f"{label}.plist was overwritten during upgrade"

    def test_uninstall(self, pkg_path):
        """fauna-uninstall removes daemons/agents + binaries, preserves data."""
        username = _get_console_user()
        user_home = _get_user_home(username)

        # Marker in the system data dir to verify preservation.
        marker = os.path.join(SYSTEM_DATA_DIR, "test-marker.txt")
        with open(marker, "w") as f:
            f.write("preserve me")

        result = subprocess.run(
            ["/usr/local/bin/fauna-uninstall", username],
            capture_output=True, text=True, timeout=30,
        )
        assert result.returncode == 0, \
            f"fauna-uninstall failed: {result.stderr}"

        # Binaries (and the FFI dylib) should be gone.
        for binary in BINARIES + DYLIBS:
            path = f"/usr/local/bin/{binary}"
            assert not os.path.exists(path), \
                f"{binary} still exists after uninstall"

        # The desktop app bundle should be gone (the user's data lives per-user in
        # ~/Library + the Keychain, never inside the bundle).
        assert not os.path.exists(APP_INSTALL_PATH), \
            f"{APP_INSTALL_PATH} still exists after uninstall"

        # System daemon plists + the per-user sync agent should be gone.
        for label in DAEMON_LABELS:
            assert not os.path.exists(f"/Library/LaunchDaemons/{label}.plist"), \
                f"{label}.plist still exists after uninstall"
        for label in AGENT_LABELS:
            plist = os.path.join(user_home, "Library", "LaunchAgents", f"{label}.plist")
            assert not os.path.exists(plist), f"{label}.plist still exists after uninstall"

        # Data directories should be preserved.
        assert os.path.isdir(SYSTEM_DATA_DIR), \
            f"{SYSTEM_DATA_DIR} removed — should be preserved"
        assert os.path.exists(marker), \
            "Marker file removed — data should be preserved"
        os.remove(marker)
