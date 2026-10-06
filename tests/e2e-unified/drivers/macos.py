from __future__ import annotations

import json
import os
import plistlib
import shlex
import shutil
import signal
import subprocess
import tempfile
import time
from pathlib import Path

from .inprocess_agent import InProcessAgentDriver
from .port_util import (
    find_free_port,
    popen_group_kwargs,
    terminate_tree,
    track_process,
    untrack_process,
)

BUNDLE_ID = "social.fauna.fauna"

# The per-instance bundle id an ARTIFACT launch stages its copy under.
#
# The `.app` bundle was abandoned for e2e in 2026-06-17 for cause, and the cause
# was NOT bundles: an app launched under the app's own FIXED `social.fauna.fauna`
# reliably stops getting a WindowServer-backed window after enough launch-and-die
# cycles (process alive, `/health` 200, zero windows, empty registry, every element
# 404) — the fleet-wide "render-death", once misdiagnosed as a VM needing a reboot.
# The bare binary never wedges because its ad-hoc identity is per-BUILD, so no two
# launches share one Launch Services identity.
#
# So the artifact suite may not launch the built bundle as-is: it stages a private
# copy per launch and rewrites `CFBundleIdentifier` to this prefix + the launch's
# own free port, restoring exactly the property the bare binary had for free. The
# default driving path is untouched and still bare-binary (`launch_mode="binary"`);
# see apple-e2e-automation.md § Registration rules rule 9 + § Artifact launch mode.
_E2E_BUNDLE_ID_PREFIX = "social.fauna.fauna.e2e"

# The sync agent's unix socket is derived from the launch's HOME —
# `<home>/Library/Application Support/Fauna/sync-agent.sock`
# (`fauna_ipc::unix_transport::macos_socket_path`), a fixed 50-byte suffix — and
# macOS caps `sockaddr_un.sun_path` at 104 bytes. That budget is what forces the
# launch root below: macOS's default `TMPDIR` is a ~48-byte
# `/var/folders/<28 opaque chars>/T/` path, so a `mkdtemp` home under it derives
# a ~134-byte socket the agent cannot bind. It spawns, dies with the bare kernel
# `path must be shorter than SUN_LEN`, and the app then reports every sync op as
# `agent unreachable` — which reads like a missing agent, not an unbindable one
# (it cost the 2026-07-24 macOS multiseat seat a full round; shared-Rust now
# rejects the path by name via `check_socket_path_len`).
#
# `/tmp` is the only root short enough to leave real headroom: 35 bytes of launch
# dir + `/home` + the 50-byte suffix = 90. It stays under the cap even when
# something resolves the `/tmp -> private/tmp` symlink (98). `mkdtemp` still
# creates the dir 0700, so per-launch isolation is unchanged.
_LAUNCH_TMP_ROOT = "/tmp"
_SUN_PATH_MAX = 104
_AGENT_SOCKET_RELPATH = "Library/Application Support/Fauna/sync-agent.sock"


# macOS pops a modal "Fauna quit unexpectedly" CrashReporter dialog whenever the
# in-process FaunaMacOS app crashes during a test (a real app bug surfaces as a
# crash, e.g. a SwiftUI view that traps). That modal blocks the *unattended*
# fleet — someone has to click it — and a crashed test then fails opaquely as a
# `BridgeDead`/timeout downstream. Suppress the dialog machine-wide for this
# user: every crash still writes a full report to ~/Library/Logs/DiagnosticReports/
# (nothing is lost — that's where you diagnose the app bug), it just doesn't pop a
# window. Idempotent + cheap; run once per process. NB ReportCrash reads the *real
# user's* CrashReporter preference via cfprefsd, NOT the per-instance throwaway
# HOME the app launches under, so this must target the driver process's own
# (real) user domain — which it does, since we don't override HOME here.
_crash_dialog_suppressed = False


def _suppress_crash_reporter_dialog() -> None:
    global _crash_dialog_suppressed
    if _crash_dialog_suppressed:
        return
    _crash_dialog_suppressed = True
    try:
        subprocess.run(
            ["defaults", "write", "com.apple.CrashReporter", "DialogType", "none"],
            check=False,
            capture_output=True,
            timeout=10,
        )
    except Exception:
        # Best-effort: at worst a dialog may appear; never break a test run over it.
        pass


# ---------------------------------------------------------------------------
# Artifact (`.app` bundle) launch support — used only by `launch_mode="bundle"`
# ---------------------------------------------------------------------------
# Module-level rather than methods so the artifact suite can assert on a bundle
# it has not launched (a DMG-mounted copy, the pristine build output) with the
# same code the driver runs.


def bundle_executable(bundle: Path) -> Path:
    """`<bundle>/Contents/MacOS/<CFBundleExecutable>`.

    Read from the Info.plist, never hardcoded: `just mac-app` names the copied
    binary **`Fauna`** while the SwiftPM product it comes from is `FaunaMacOS`,
    so a hardcoded product name resolves to a path that does not exist in a real
    bundle. (That was the latent defect in this driver's back-compat bundle
    branch — it had no caller to expose it.)
    """
    name = _plist_read(bundle / "Contents" / "Info.plist", "CFBundleExecutable")
    if not name:
        raise RuntimeError(
            f"{bundle}/Contents/Info.plist declares no CFBundleExecutable — "
            f"not a launchable app bundle."
        )
    return bundle / "Contents" / "MacOS" / name


def read_bundle_id(bundle: Path) -> str:
    """The bundle's `CFBundleIdentifier` as it sits on disk."""
    return _plist_read(bundle / "Contents" / "Info.plist", "CFBundleIdentifier") or ""


def _plist_read(plist: Path, key: str) -> str | None:
    """Read one scalar out of an Info.plist — via `plistlib`, not `plutil`.

    `plistlib` reads both the binary and XML forms, so this is behaviour-for-
    behaviour what `plutil -extract <key> raw` gave, minus the subprocess and
    minus the macOS-only tool. That last part is the point: the two callers
    above are also exercised by `test_macos_artifact_launch_mode.py`, a tier_1
    file whose own docstring promises it "needs no build, no bundle, and no
    macOS" — a promise the `plutil` shell-out broke, reddening three tests in
    every non-macOS tier_1 sweep with `FileNotFoundError: 'plutil'` (found
    2026-08-13). A missing file, an unparseable plist, or an absent key all
    answer None, exactly as a non-zero `plutil` exit did.
    """
    try:
        with open(plist, "rb") as fh:
            value = plistlib.load(fh).get(key)
    except (OSError, plistlib.InvalidFileException):
        return None
    return str(value).strip() if value is not None else None


def _bundle_entitlements(bundle: Path, dest: Path) -> Path | None:
    """Extract the bundle's current entitlements to `dest`; None if it has none.

    Re-signing with `--entitlements` is not optional cosmetics: the shipped app is
    signed with `Fauna-macOS.entitlements`, and re-signing without them would
    silently produce an artifact with a DIFFERENT sandbox/capability posture than
    the one users run — the exact class of divergence this suite exists to catch.
    Extracting from the bundle (rather than reading the repo's .entitlements file)
    keeps the staged copy faithful to what was actually built, with no repo-root
    dependency.
    """
    out = subprocess.run(
        ["codesign", "-d", "--entitlements", "-", "--xml", str(bundle)],
        capture_output=True, text=True,
    )
    xml = out.stdout
    start = xml.find("<?xml")
    if out.returncode != 0 or start < 0:
        return None
    dest.write_text(xml[start:])
    return dest


def stage_bundle_with_instance_id(source: Path, dest_dir: Path, instance: str) -> Path:
    """Copy `source` into `dest_dir` and give the copy a per-instance bundle id.

    Returns the staged `.app`. Three steps, all load-bearing:

    1. **Copy** — the build output is shared by every concurrent launch, so the id
       rewrite has to land on a private copy or launches would fight over one file.
       `cp -Rc` (APFS copy-on-write clone), not `shutil.copytree`: a debug bundle is
       ~570 MB, which clones in ~30 ms and costs no extra disk until written, versus
       seconds and a real 570 MB per launch — and macOS's chronic near-ENOSPC makes
       the disk half of that a fleet hazard, not just a slow test.
    2. **Rewrite `CFBundleIdentifier`** to `_E2E_BUNDLE_ID_PREFIX.<instance>`, which
       is what keeps repeated artifact launches off the one fixed Launch Services
       identity that wedges WindowServer (see `_E2E_BUNDLE_ID_PREFIX`).
    3. **Re-sign the OUTER bundle ad-hoc, carrying the original entitlements
       forward, never `--deep`** — editing the Info.plist invalidates the app's
       signature, and macOS refuses to launch a bundle whose signature does not
       match its contents. Nested code is left exactly as `mac-app` signed it, so
       this reproduces the shipped signing shape rather than inventing one; see
       the comment at the `codesign` call for why `--deep` would break that.
    """
    # `cp` will not create intermediate directories, and a missing parent is the
    # difference between a ~30 ms clone and a silent ~570 MB fallback copy that
    # still passes — a slow green nobody investigates.
    dest_dir.mkdir(parents=True, exist_ok=True)
    dest = dest_dir / source.name
    if dest.exists():
        shutil.rmtree(dest)
    clone = subprocess.run(
        ["cp", "-Rc", str(source), str(dest)], capture_output=True, text=True,
    )
    if clone.returncode != 0:
        # Non-APFS volume (or a cross-volume dest): fall back to a real copy rather
        # than failing — correctness first, the clone is only an optimisation.
        # Clear any partial tree `cp` left behind, else copytree refuses outright.
        if dest.exists():
            shutil.rmtree(dest)
        shutil.copytree(source, dest, symlinks=True)

    plist = dest / "Contents" / "Info.plist"
    subprocess.run(
        ["plutil", "-replace", "CFBundleIdentifier", "-string",
         f"{_E2E_BUNDLE_ID_PREFIX}.{instance}", str(plist)],
        check=True, capture_output=True, text=True,
    )

    # NEVER `--deep`. Only the app's own Info.plist changed, so only the app's
    # signature was invalidated; the nested code (the bundled `fauna-sync-agent`,
    # `Contents/PlugIns/*.appex`, `Sparkle.framework`) is untouched and keeps the
    # signature `mac-app` gave it, which is the whole point — those signatures
    # carry each nested bundle's OWN entitlements. `codesign --deep --entitlements`
    # re-stamps every nested Mach-O with the OUTER entitlements (measured
    # 2026-08-25, `installer/macos/sign-app-bundle.sh`), which would hand the
    # sandboxed File Provider extension the app-only account keychain group and
    # make this suite's staged copy a bundle NO user ever runs — the exact
    # divergence it exists to catch. Re-signing the outer bundle alone re-seals
    # those nested signatures by reference, and `--verify --deep --strict` still
    # passes.
    ents = _bundle_entitlements(source, dest_dir / "entitlements.plist")
    sign = ["codesign", "--force", "--sign", "-"]
    if ents is not None:
        sign += ["--entitlements", str(ents)]
    subprocess.run(sign + [str(dest)], check=True, capture_output=True, text=True)
    return dest


# ---------------------------------------------------------------------------
# The photo-library venue (`launch_mode="photo-library"`) — convention 12's macOS arm
# ---------------------------------------------------------------------------
# Every other macOS launch is isolated from the box (convention 10), and that
# isolation cannot reach PhotoKit: the System Photo Library is served by the
# per-user `photolibraryd` in the GUI domain, which resolves it daemon-side, so no
# HOME/CFFIXED_USER_HOME redirect changes what `PHAsset.fetchAssets` sees. The
# photo-backup witnesses therefore run against the VM's REAL (disposable, user-
# approved 2026-09-26) library, from `tests/real_session/` only, behind that
# directory's three gates. Two things make that launch different from every other:
#
# 1. **A stable signed identity.** TCC keys a Photos grant on the code's designated
#    requirement. The bare binary is ad-hoc signed, so its requirement is its cdhash
#    and every rebuild would need a new human "Allow". The venue wraps the bare
#    binary in a minimal `.app` under ONE fixed bundle id and signs it with the
#    login keychain's Apple Development identity, whose requirement (identifier +
#    certificate leaf) survives rebuilds — so the grant is given once.
# 2. **A Launch Services launch.** A process Popen'd from pytest inherits its
#    RESPONSIBLE process from the terminal the run started in, and TCC charges the
#    request to that terminal — granting it would hand Photos to everything the
#    terminal ever spawns. `open` makes the app responsible for itself. The price is
#    that the app is not our child: the driver finds it by its automation port and
#    ends it by pid (`_LaunchedApp`), and sweeps a crashed run's leftover at the next
#    launch (`_PHOTO_LIBRARY_PIDFILE`).
#
# ⚠ The fixed id reintroduces the one property the bare binary avoids (the
# fixed-id WindowServer wedge above `_E2E_BUNDLE_ID_PREFIX`). It is accepted here
# because TCC needs exactly that stability and the venue runs a handful of launches
# per pass, not a sweep's hundreds; a run that stops getting windows names the
# wedge in `assert_render_ready`'s failure.
#
# Owner: `e2e-conventions.md` convention 12 (the macOS arm).

#: The one bundle id the photo-library venue's grant is given to. Never derived
#: from `BUNDLE_ID`: the grant must NOT be the shipped app's.
PHOTO_LIBRARY_BUNDLE_ID = "social.fauna.fauna.e2e.photos"
_PHOTO_LIBRARY_APP_NAME = "FaunaE2EPhotos.app"
_REPO_ROOT = Path(__file__).resolve().parents[3]
_MACOS_INFO_PLIST = _REPO_ROOT / "apps/fauna-apple/Fauna-macOS/Resources/Info.plist"
#: Stable per checkout, so Launch Services sees one path per checkout rather than
#: one per launch, and the signature (the step that can need a human) is redone
#: only when the binary changed.
_PHOTO_LIBRARY_STAGE_DIR = _REPO_ROOT / "apps/fauna-apple/.build/e2e-photo-library"
_PHOTO_LIBRARY_PIDFILE = _PHOTO_LIBRARY_STAGE_DIR / "running.pid"
#: How long a signature may take before it is read as the keychain's key-access
#: prompt waiting for a human (a first use of the key by `codesign` asks once).
#: Long enough for a person running `just mac-photos-e2e-grant` at the screen to
#: type the login password; an unattended run past it fails naming that recipe.
_CODESIGN_TIMEOUT_S = 120


def apple_development_identity() -> str:
    """The SHA-1 of the login keychain's first valid Apple Development identity.

    Returned and used as the hash, never the name: the certificate's common name
    carries a person's name, and no person's name may reach a log, a failure
    message or a file.
    """
    out = subprocess.run(
        ["security", "find-identity", "-v", "-p", "codesigning"],
        capture_output=True, text=True,
    ).stdout
    for line in out.splitlines():
        parts = line.split()
        # `  1) <40-hex SHA-1> "Apple Development: …"`
        if len(parts) >= 3 and parts[0].endswith(")") and '"Apple Development:' in line:
            return parts[1]
    raise RuntimeError(
        "no valid Apple Development code-signing identity in the login keychain — "
        "the photo-library venue signs its test bundle with one so the one-time "
        "Photos grant survives rebuilds (drivers/macos.py § the photo-library venue)"
    )


def _photo_library_stamp(binary: Path) -> dict:
    sparkle = binary.parent / "Sparkle.framework"
    def mark(p: Path):
        st = p.stat()
        return [st.st_size, st.st_mtime_ns]
    return {
        "binary": mark(binary),
        "info_plist": mark(_MACOS_INFO_PLIST),
        "sparkle": mark(sparkle) if sparkle.exists() else None,
        "bundle_id": PHOTO_LIBRARY_BUNDLE_ID,
    }


def stage_photo_library_bundle(binary: Path, stage_dir: Path | None = None) -> Path:
    """Wrap the bare `FaunaMacOS` build in a minimal `.app` under
    `PHOTO_LIBRARY_BUNDLE_ID`, signed with the Apple Development identity.

    Reused as-is while the binary, the Info.plist source and the embedded Sparkle
    are unchanged (a stamp beside the bundle), because signing is the one step that
    can stop for a human. The bundle is exactly what the bare launch runs — the same
    executable, no entitlements (the bare binary has none) — plus the three things a
    bundle adds: an Info.plist carrying `NSPhotoLibraryUsageDescription` (TCC
    refuses to prompt for a process without one), the fixed id, and the stable
    signature.
    """
    stage_dir = stage_dir or _PHOTO_LIBRARY_STAGE_DIR
    app = stage_dir / _PHOTO_LIBRARY_APP_NAME
    stamp_file = stage_dir / "stamp.json"
    stamp = _photo_library_stamp(binary)
    if app.exists() and stamp_file.exists():
        try:
            same = json.loads(stamp_file.read_text()) == stamp
        except (OSError, ValueError):
            same = False
        verified = subprocess.run(
            ["codesign", "--verify", "--strict", str(app)], capture_output=True,
        ).returncode == 0
        if same and verified:
            return app

    if app.exists():
        shutil.rmtree(app)
    macos_dir = app / "Contents" / "MacOS"
    frameworks = app / "Contents" / "Frameworks"
    macos_dir.mkdir(parents=True)
    frameworks.mkdir(parents=True)
    exe = macos_dir / binary.name
    shutil.copy2(binary, exe)
    # The bare binary finds Sparkle through `@loader_path` (it sits beside it in the
    # build dir); inside a bundle the framework lives in `Contents/Frameworks`, so
    # add the standard rpath. The signature this invalidates is replaced below.
    sparkle = binary.parent / "Sparkle.framework"
    if sparkle.exists():
        subprocess.run(["cp", "-R", str(sparkle), str(frameworks)], check=True,
                       capture_output=True)
        subprocess.run(
            ["install_name_tool", "-add_rpath", "@executable_path/../Frameworks", str(exe)],
            check=True, capture_output=True,
        )

    with open(_MACOS_INFO_PLIST, "rb") as fh:
        info = plistlib.load(fh)
    info.update({
        "CFBundleIdentifier": PHOTO_LIBRARY_BUNDLE_ID,
        "CFBundleExecutable": binary.name,
        "CFBundleName": "Fauna E2E Photos",
        "CFBundleDisplayName": "Fauna E2E Photos",
        # A test bundle must never check an appcast (the app already keeps the
        # updater off in DEBUG; this is belt and braces for the bundle's own keys).
        "SUEnableAutomaticChecks": False,
    })
    with open(app / "Contents" / "Info.plist", "wb") as fh:
        plistlib.dump(info, fh)

    identity = apple_development_identity()
    try:
        signed = subprocess.run(
            ["codesign", "--force", "--timestamp=none", "--sign", identity, str(app)],
            capture_output=True, text=True, timeout=_CODESIGN_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired:
        raise RuntimeError(
            f"codesign did not finish within {_CODESIGN_TIMEOUT_S}s signing the "
            "photo-library test bundle. That is the login keychain asking whether "
            "`codesign` may use the Apple Development key — a one-time dialog on the "
            "Mac's screen. Click 'Always Allow' there (it will not ask again), then "
            "re-run: `just mac-photos-e2e-grant`."
        ) from None
    if signed.returncode != 0:
        raise RuntimeError(
            f"codesign failed signing the photo-library test bundle "
            f"(rc={signed.returncode}): {signed.stderr.strip()[-600:]}"
        )
    subprocess.run(["codesign", "--verify", "--strict", str(app)], check=True,
                   capture_output=True)
    stamp_file.write_text(json.dumps(stamp))
    return app


class _LaunchedApp:
    """A `subprocess.Popen`-shaped handle on an app Launch Services started.

    `open` returns before the app is up and the app is launchd's child, not ours,
    so there is no Popen to hold. This answers the four things `terminate_tree`,
    `track_process` and the driver's liveness probes use — `pid`, `poll`, `wait`,
    `terminate`/`kill` — by pid. Liveness is `kill(pid, 0)` on OUR app's pid only
    (found by its automation port and checked against the staged bundle's path),
    never a name match that could touch another session's process.
    """

    def __init__(self, pid: int):
        self.pid = pid
        self.returncode: int | None = None

    def poll(self) -> int | None:
        if self.returncode is not None:
            return self.returncode
        try:
            os.kill(self.pid, 0)
            return None
        except ProcessLookupError:
            self.returncode = -1
            return self.returncode
        except PermissionError:
            return None

    def wait(self, timeout: float | None = None) -> int:
        deadline = None if timeout is None else time.monotonic() + timeout
        while self.poll() is None:
            if deadline is not None and time.monotonic() > deadline:
                raise subprocess.TimeoutExpired(f"pid {self.pid}", timeout)
            time.sleep(0.1)
        return self.returncode  # type: ignore[return-value]

    def terminate(self) -> None:
        self._signal(signal.SIGTERM)

    def kill(self) -> None:
        self._signal(signal.SIGKILL)

    def _signal(self, sig: int) -> None:
        try:
            os.kill(self.pid, sig)
        except ProcessLookupError:
            pass


def _pid_listening_on(port: int) -> int | None:
    out = subprocess.run(
        ["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-t"],
        capture_output=True, text=True,
    ).stdout.split()
    return int(out[0]) if out else None


def _pid_runs_bundle(pid: int, app: Path) -> bool:
    comm = subprocess.run(["ps", "-o", "comm=", "-p", str(pid)],
                          capture_output=True, text=True).stdout.strip()
    # `ps` prints the resolved path (`/private/tmp/...`) while a staged bundle is
    # named through the `/tmp` symlink — compare like with like.
    return os.path.realpath(comm).startswith(os.path.realpath(str(app)))


def _sweep_leftover_photo_library_app(app: Path) -> None:
    """End a previous run's photo-library app that outlived its pytest.

    A Launch Services launch is not in the run's process group, so neither the
    orderly `track_process` teardown nor the pipe-EOF reaper covers a SIGKILLed
    run. The pidfile names the one process this checkout's venue started; it is
    ended only if it still runs THIS checkout's staged bundle (pid reuse cannot
    make it touch anything else), and the venue's machine-wide lock means no live
    run owns it.
    """
    try:
        pid = int(_PHOTO_LIBRARY_PIDFILE.read_text().strip())
    except (OSError, ValueError):
        return
    if _pid_runs_bundle(pid, app):
        terminate_tree(_LaunchedApp(pid))
    try:
        _PHOTO_LIBRARY_PIDFILE.unlink()
    except OSError:
        pass


# ---------------------------------------------------------------------------
# In-process automation agent (the sole macOS/iOS e2e driver) — the former
# XCUITest/AutomationMode bridge (`MacosBridgeDriver`) was retired at cutover.
# ---------------------------------------------------------------------------
# The macOS app hosts an automation HTTP server *inside its own process*
# (`InProcessAutomationServer` + the `AutomationRegistry` driven by the
# `automation*` view modifiers), bound to a per-instance FAUNA_E2E_AGENT_PORT —
# the exact shape the Linux app already uses (`LinuxBridgeDriver`). This
# driver launches the bundled FaunaMacOS binary directly with that port + an
# isolated HOME and talks straight to its `/element/*` + `/app/*` surface (the
# standard `HttpBridgeDriver` contract). NO xcodebuild, NO XCUITest, NO
# machine-wide AutomationMode → nothing to wedge (no reboots) and per-instance
# isolation (concurrent macOS e2e runs stop contending — serialization gone).
#
# Authority + Phase-0 findings: docs/goal/architecture/apps/apple-e2e-automation.md.
# The sole macOS e2e driver: the legacy XCUITest path + apple-bridge runner were
# deleted at the cutover once iOS reached in-process parity.
#
# This is the apple-side twin of `LinuxBridgeDriver`. The post-click settle, the
# `/health` probe, and `recover()` — identical to the iOS in-process driver — now
# live in the shared `InProcessAgentDriver` base (the extraction iOS joining in
# Phase 3 triggered); only the macOS-specific process model (a direct `FaunaMacOS`
# Popen with an isolated HOME) stays here. Linux keeps its own `LinuxBridgeDriver`
# (a Rust in-app agent, no shared Python skeleton to lift — and refactoring the
# live linux driver would be needless risk).


class MacosInProcessDriver(InProcessAgentDriver):
    """macOS E2E driver backed by the in-process automation server.

    Each `launch()` spawns a fresh, fully-isolated FaunaMacOS process and drives
    it over its in-app HTTP server. `teardown()` kills it; the inherited
    `recover()` relaunches a wedged one so a single bad test costs one test, not
    the rest of the session.
    """

    # The in-process automation server serves POST /element/scroll-into-view for
    # real (`InProcessAutomationServer.swift` -> `AutomationRegistry
    # .resolvedScrollIntoView`): it centres the element in every enclosing scroll
    # view by walking up from its registration sentinel. Flipped True only when
    # that route stopped being a no-op stub -- flipping it alone would have bought
    # a silent no-op, the exact rule-11 hazard this route was carried as.
    _supports_scroll_into_view = True

    #: The CURRENT app process's home (CFFIXED_USER_HOME target; see `launch`).
    _home: str | None = None
    #: The at-rest twin of `_preserved_cred_dir`: set by
    #: `preserve_state_across_relaunch()` so a relaunch reads back the previous
    #: process's Application Support (MLS db, SwiftData) too, not just its keychain.
    _preserved_home: str | None = None
    #: Where the CURRENT app process saves e2e-mode downloads (see `launch`
    #: and `download_dir()`).
    _download_dir: str | None = None
    _widget_dir: str | None = None
    #: This launch's isolated `Library/Application Support`, or None when the
    #: launch did not relocate it (see `app_support_dir()`).
    _app_support: str | None = None
    #: This launch's STAGED `.app`, or None for the default bare-binary launch
    #: (see `bundle_path()` and `launch()`'s `launch_mode`).
    _bundle_path: str | None = None
    #: The environment this launch's process was handed, kept so a LaunchServices
    #: relaunch of the same bundle (`relaunch_through_launch_services()`) can pass
    #: the same isolation (HOME, CFFIXED_USER_HOME, credential dir, agent port)
    #: through `open --env` — `open` hands the app launchd's environment, never ours.
    _launch_env: dict | None = None

    def log_scope_across_relaunch(self) -> str:
        """``"per-launch"`` — `_tmp_dir` is assigned unconditionally in `launch()`, so no preserve pin can reuse the previous launch's `app.err`.

        See the base declaration for what each answer means and why it is
        declared rather than inferred; pinned per driver by
        `tests/test_module_relaunch.py`.
        """
        return "per-launch"

    def is_mobile(self) -> bool:
        return False

    def reset(self, timeout: float = 10.0) -> None:
        """Between-tests reset. Also DROPS any credential-dir pin.

        The driver is session-scoped (`conftest._driver_cache`), so a pin set by one test's
        `preserve_state_across_relaunch()` would otherwise still be in force for the next —
        which relaunches into the previous test's store, including its pending-factory-reset
        slot, pointing at a nest that no longer exists. The pin is per-test by construction:
        the fixture calls `reset()` before the test body, and `recover()` (teardown+launch)
        never calls `reset()`, so a pin taken inside the test survives exactly as long as it
        should.
        """
        self._preserved_cred_dir = None
        self._preserved_home = None
        super().reset(timeout=timeout)

    def preserve_state_across_relaunch(self) -> bool:
        """Pin this launch's E2E keychain so a later `recover()` / `hard_reload()`
        relaunch reads back what this process wrote. See the base contract.

        Off by default (see `launch`): each launch normally gets a fresh credential
        dir, so the app's E2E keychain starts empty — which is what most journeys
        want, and load-bearing for the mid-claim journey (it wants the relaunched app
        to show create-identity).

        A test asserting *client-side* durability calls this first. The
        pending-factory-reset slot (gaps CR-1 / CR-2, `common.md` § Client-state
        recoverability) is the case it exists for: the claim code is minted and
        persisted before the reset is dispatched, and if the harness discarded the
        store on relaunch the test would "prove" a resume no real user gets.

        The dirs already exist (this launch made them) and teardown does not delete
        them, so pinning them into the next launch config is enough. Mirrors linux.

        Both halves of the store are pinned: the keychain (credential dir) AND the
        at-rest home (Application Support — MLS db, SwiftData). A durability test
        asserts the whole client store survives, and since the unconditional
        `CFFIXED_USER_HOME` isolation each launch's at-rest half lives in its own
        throwaway home — without the home pin a relaunch would silently start from
        an empty store and "prove" a persistence no real user gets.
        """
        if self._cred_dir is None:
            return False
        self._preserved_cred_dir = self._cred_dir
        self._preserved_home = self._home
        return True

    def app_support_dir(self) -> str | None:
        """This launch's ``Library/Application Support`` — where the app's
        account-scoped stores land (``Fauna/<actor-id-hex>/mls.db``;
        `account-scoping.md` § Serialized switching — ``segment-backup`` was
        one such store too until the in-app coordinator was deleted fleet-wide
        2026-08-15, `backup-restore.md` § Background Tasks → *Flip status
        (slice 5)*). The iOS twin resolves the simulator's app container instead,
        so an at-rest assertion reads the same way on both apple apps.

        ``launch()`` relocates the store unconditionally (``CFFIXED_USER_HOME``
        points at the per-launch throwaway home, or the pinned ``home`` when a
        caller passed one), so this is always set once launched. ``None`` only
        before the first launch. The REAL user profile is never the store.
        """
        return self._app_support

    @property
    def sync_agent_state_base(self) -> str | None:
        """Where THIS launch's `fauna-sync-agent` keeps its state:
        `<launch HOME>/Library/Application Support/Fauna/sync`, flat — the agent
        re-scopes under `<that>/<actor-id-hex>/` itself once it is provisioned,
        which `helpers/sync_agent_config.py` already globs.

        The agent is a child the app spawns under the `real_sync_agent` marker, and
        it inherits this launch's relocated HOME, so its user-domain root is the
        launch's own (`bins/fauna-sync-agent/src/config.rs::SyncPaths::base_dir`)
        — **not** the shared app-group container it used before 2026-08-25. That
        is exactly the `_resolved_store_root` this driver already publishes for
        the relaunch carry, so one field answers both readers rather than two
        copies of the derivation drifting.

        ``None`` before the first launch, which `agent_state_base` names loudly.
        The unix drivers answer it only when the state root is not derivable from
        `config_home`; apple has no `config_home` at all, so it always answers.
        """
        return getattr(self, "_resolved_store_root", None)

    def is_macos(self) -> bool:
        return True

    # ------------------------------------------------------------------
    # Window-level ops (mirrors linux/windows drivers' window_close/
    # is_app_alive; the InProcessAutomationServer's `/window/close` route
    # owns the AppKit `performClose` mechanism)
    # ------------------------------------------------------------------
    def window_close(self) -> None:
        """Simulate the titlebar close button on every visible window —
        AppKit's `performClose`, run on the real app's main actor via the
        in-process automation server. Tolerates a dropped connection: a
        close that leads the app to decide to terminate can exit the process
        (and race the HTTP reply) before this call would otherwise return,
        exactly like the windows driver's own `/window/close` twin. Assert
        the outcome via `is_app_alive()` or by polling whatever the close was
        meant to prove (e.g. the sync agent's own socket).
        """
        try:
            self._post("/window/close", {})
        except Exception:
            pass

    def relaunch_with_args(self, app_args: str) -> bool:
        """`recover()` — force-quit + relaunch — with `app_args` on the new
        process's argv (e.g. `--autostart`, the flag the auto-start LaunchAgent
        passes). Pair with `preserve_state_across_relaunch()` to come back up
        signed in. True when the relaunch came up."""
        config = getattr(self, "_launch_config", None)
        if config is None:
            return False
        self._launch_config = {**config, "app_args": app_args}
        return self.recover()

    def visible_windows(self) -> list[str]:
        """Identifiers of the app's on-screen windows that can become main —
        the menu-bar status item and its popover excluded, since a resident app
        always has those. Empty is what a hidden auto-start launch promises
        (`apps/macos.md` § App Lifecycle → *Auto-start at sign-in*). Served by
        the in-process automation server's `GET /window/visible`."""
        return list(self._get("/window/visible").get("windows", []))

    def quit_app(self) -> None:
        """Quit the app the way ⌘Q does — `NSApp.terminate(nil)`, run on the
        real app's main actor by the in-process automation server, so the real
        `applicationShouldTerminate` gate (and its bounded leave-flush) runs.

        **Not** `window_close()`: on macOS a window close is deliberately never
        a quit (`test_sync_agent_survives_macos_window_close.py`,
        `docs/goal/architecture/apps/macos.md` § Sync), so `performClose` never
        reaches the leave-flush door. This is macOS's leave door; linux's and
        windows' is their window close, tui's its `exit-tab`, web's `pagehide`
        (`reserved-folders.md` § The leave-flush promise).

        Tolerates a dropped connection for the same reason `window_close` does:
        the process can exit before the HTTP reply lands. Assert the outcome via
        `wait_app_exit()`.
        """
        try:
            self._post("/app/lifecycle/quit", {})
        except Exception:
            pass

    def is_app_alive(self) -> bool:
        """True while the launched FaunaMacOS process is still running.

        Named to mirror the linux/windows/tui drivers' `is_app_alive()`."""
        proc = getattr(self, "_app_proc", None)
        return proc is not None and proc.poll() is None

    def wait_app_exit(self, timeout: float = 10.0) -> bool:
        """Block until the app process exits; True if it exited within `timeout`."""
        proc = getattr(self, "_app_proc", None)
        if proc is None:
            return True
        try:
            proc.wait(timeout=timeout)
            return True
        except subprocess.TimeoutExpired:
            return False

    # ----------------------------------------------------------------------
    # Process lifecycle
    # ----------------------------------------------------------------------
    def _executable(self, config: dict) -> Path:
        """The bare swift-build FaunaMacOS executable. `_build_app_config`
        (conftest) now passes the bare `.build/.../debug/FaunaMacOS` path
        directly — NOT the ~/Applications .app bundle (that bundle is vestigial
        XCUITest scaffolding whose fixed bundle ID caused the macOS "render-death";
        see the conftest macos branch). We launch it directly (not via
        `open`/Launch Services) so the per-instance env (the agent port, the
        isolated HOME) reaches the process and we own its PID for teardown.

        A `.app` handed here resolves through `bundle_executable` — the artifact
        mode's path, and also the back-compat branch for a caller that still hands
        a bundle."""
        app_path = Path(config["app_path"])
        if app_path.suffix == ".app" or (app_path / "Contents" / "MacOS").exists():
            return bundle_executable(app_path)
        return app_path  # already a bare executable

    def bundle_path(self) -> str | None:
        """This launch's STAGED `.app` copy, or None for a bare-binary launch.

        The artifact suite reads it back to assert on the thing that actually ran
        (its rewritten bundle id, its embedded frameworks) rather than on the
        pristine build output, which is not what the process was launched from.
        """
        return self._bundle_path

    def download_dir(self) -> str | None:
        """Where FaunaApp saves e2e-mode downloads (no save dialog under e2e).

        Mirrors the app-side seam (`SnapshotFileSaver.e2eDownloadDir`) and
        windows' `WindowsBridgeDriver.download_dir()`: set fresh every
        `launch()` (see `_download_dir`), so this is only meaningful after a
        launch has happened.
        """
        return self._download_dir

    def widget_dir(self) -> str | None:
        """Where this launch's app writes the home-screen widget snapshot
        (`FAUNA_E2E_WIDGET_DIR`, fresh every `launch()`). Never the app-group
        container: that resolves to the REAL home even under
        `CFFIXED_USER_HOME`, i.e. machine-global state.
        """
        return self._widget_dir

    def _seed_app_support(self, config: dict) -> None:
        """Copy ``config["seed_app_support"]`` — ``{relpath: local file}`` — into this
        launch's Application Support, before the process starts.

        The at-rest fixture seam: it is how a test stands up "this install already
        existed, in the PRE-upgrade layout" (a flat ``Fauna/conv-mls.db``) without
        needing a previous build to write it. Parity with `seed_credentials`, which
        does the same for the credential store; the iOS twin seeds the simulator's
        app container instead. A no-op when absent.
        """
        seed = config.get("seed_app_support")
        if not seed or not self._app_support:
            return
        for rel, source in seed.items():
            target = Path(self._app_support) / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)

    def launch(self, config: dict) -> None:
        # Keep a crashing app from popping a modal CrashReporter dialog that
        # stalls the unattended fleet (see module note). Once per process.
        _suppress_crash_reporter_dialog()

        self._launch_config = config
        port = find_free_port()
        self._agent_port = port
        self._url = f"http://127.0.0.1:{port}"

        # `launch_mode`: "binary" (default — every ordinary apple e2e test) launches
        # the bare swift-build executable exactly as before. "bundle" is the ARTIFACT
        # mode: the subject becomes the real `.app` that `just mac-app` assembles and
        # `mac-dmg`/the installer .pkg ship, staged per launch (see
        # `stage_bundle_with_instance_id`). The default path below is untouched by
        # design — every apple e2e test in the fleet rides it.
        #
        # "photo-library" is the real-session photo-backup venue (the section above
        # `apple_development_identity`): the bare binary wrapped in a stably signed
        # `.app` under `PHOTO_LIBRARY_BUNDLE_ID`, launched through Launch Services,
        # against the REAL System Photo Library. Reachable only from
        # `tests/real_session/` (the `real_photos_library` marker is what selects it,
        # `conftest._build_app_config`).
        launch_mode = config.get("launch_mode", "binary")
        if launch_mode not in ("binary", "bundle", "photo-library"):
            raise RuntimeError(
                f"unknown launch_mode {launch_mode!r} "
                "(expected 'binary', 'bundle' or 'photo-library')"
            )
        self._launch_mode = launch_mode
        exe = self._executable(config)
        if not exe.exists():
            hint = (
                "Run 'just mac-app debug' to build the bundle"
                if launch_mode == "bundle"
                else "Run 'just mac-debug' to build it"
            )
            raise RuntimeError(
                f"macOS executable not found at {exe}.\n{hint}, then re-run tests."
            )

        # Per-app isolation: a fresh HOME so the app's Application Support
        # (SwiftData store, logs) and any UserDefaults land in a throwaway
        # sandbox — concurrent runs never collide, and each starts clean. The
        # keychain never touches the real one either (the app's KeychainStore
        # E2E gate, keyed on FAUNA_E2E_AGENT_PORT).
        #
        # `config["home"]` PINS it instead. That is the HOME-reuse seam: it hands ONE
        # persistent client state root (Application Support — the SwiftData store and
        # the MLS state db that only this client can decrypt with) to two *different
        # builds* in sequence, which is the whole question the client at-rest
        # upgrade-in-place grid asks. Mirrors linux's `xdg_base`; unset, the behaviour
        # is exactly the fresh-per-launch default above. The relaunch pin
        # (`preserve_state_across_relaunch()`) sits between the two: same build,
        # same store, next process — and an explicit `config["home"]` outranks it
        # for the same reason `credential_dir` outranks the credential pin below.
        tmp = tempfile.mkdtemp(prefix="fauna-e2e-macos-agent-", dir=_LAUNCH_TMP_ROOT)
        self._tmp_dir = tmp

        # Artifact mode: stage a private copy of the `.app` under this launch's tmp
        # and give it a per-instance `CFBundleIdentifier`, then launch the copy's
        # inner executable. Launching the inner executable (rather than `open`) is
        # what keeps the per-instance env and PID ownership this driver depends on,
        # while `Bundle.main` still resolves to the staged `.app` — so Info.plist
        # keys, `Contents/Resources`, and the embedded `Contents/Frameworks` are all
        # the shipped artifact's, not the bare binary's.
        if launch_mode == "bundle":
            staged = stage_bundle_with_instance_id(
                Path(config["app_path"]), Path(tmp), f"{os.getpid()}-{port}"
            )
            self._bundle_path = str(staged)
            exe = bundle_executable(staged)
        elif launch_mode == "photo-library":
            staged = stage_photo_library_bundle(exe)
            _sweep_leftover_photo_library_app(staged)
            self._bundle_path = str(staged)
        else:
            self._bundle_path = None
        home = config.get("home") or self._preserved_home or os.path.join(tmp, "home")
        os.makedirs(home, exist_ok=True)
        self._home = home
        # A launch that will spawn the real sync agent must derive a bindable
        # socket path (see `_LAUNCH_TMP_ROOT`). The default home always fits; a
        # caller-PINNED `config["home"]` can still overshoot — pytest's own
        # `tmp_path` is ~90 bytes — so fail here, naming the budget, rather than
        # three phases later as an opaque "agent unreachable". Gated on the
        # real-agent flag because the many launches that never start an agent are
        # legitimately free to pin a long home.
        if config.get("environment", {}).get("FAUNA_E2E_REAL_SYNC_AGENT"):
            sock = os.path.join(home, _AGENT_SOCKET_RELPATH)
            if len(sock.encode()) >= _SUN_PATH_MAX:
                raise RuntimeError(
                    f"launch HOME {home!r} derives a {len(sock.encode())}-byte sync-agent "
                    f"socket path, over macOS's sun_path limit of {_SUN_PATH_MAX - 1} usable "
                    f"bytes:\n  {sock}\nThe agent would spawn and die at bind "
                    f"('path must be shorter than SUN_LEN'), and the app would report only "
                    f"'agent unreachable'. Pin a shorter config['home'] (under "
                    f"{_LAUNCH_TMP_ROOT}), or drop the real-agent marker."
                )

        # The keychain — the identity half of the at-rest seam, where the state half
        # rides on `home` above — resolves in the store half both apple drivers share.
        cred_dir = self._resolve_credential_store(config, tmp)

        env = dict(os.environ)
        env.update(config.get("environment", {}))
        # The resident engine's full-reconcile cadence
        # (`always_resident::rescan_interval`, the compile-gated
        # `FAUNA_E2E_RESCAN_MS` seam) — linux's and tui's twin of this launch
        # already default it (`drivers/linux.py`, `drivers/tui.py`); this one
        # never did, so a macOS launch ran the 300 s production cadence with no
        # test able to see or budget for it. Inherited by the spawned
        # `fauna-sync-agent` child (`ChildSpawner::spawn`'s bare
        # `Command::new`, no `env_clear`) since macOS's actual engine runs
        # there post-A4-cutover, never in FaunaMacOS itself. A caller-supplied
        # override in `config["environment"]` (merged above) wins.
        env.setdefault("FAUNA_E2E_RESCAN_MS", "30000")
        env["HOME"] = home
        # macOS resolves `.applicationSupportDirectory` — where the MLS state db
        # (`conv-mls.db`, `mls.db`) and the SwiftData store live — from CoreFoundation's
        # notion of the home dir, which honors `CFFIXED_USER_HOME` but *not* `HOME`
        # (measured: with only `HOME` relocated, both `.applicationSupportDirectory` and
        # `NSHomeDirectory()` stay at the real `~`, so the store lands in the real
        # profile). So the `HOME` line above isolates env/argv-derived paths only;
        # `CFFIXED_USER_HOME` is what makes the launch a genuinely separate *install*.
        # It is set UNCONDITIONALLY: an e2e launch must never share the real profile's
        # install-scoped base — account identity cannot substitute for this (sign-out's
        # erasure sweeps every actor dir on the install by design, and SwiftData /
        # UserDefaults are install-scoped), so isolation comes from the launch
        # (account-scoping.md § Testing interplay; regression gate:
        # tests/test_macos_launch_isolation.py).
        env["CFFIXED_USER_HOME"] = home
        self._app_support = os.path.join(home, "Library", "Application Support")
        # The unified account-store root THIS launch resolved — shared Rust's
        # `platform_state_base()` under the relocated HOME — published after the
        # carry began in `_resolve_credential_store` above, so the harvest there
        # read the launch being replaced; the carry restores the signing-in
        # actor's replica under it beside the slot.
        self._resolved_store_root = os.path.join(self._app_support, "Fauna", "sync")
        env["FAUNA_E2E_AGENT_PORT"] = str(port)
        env["FAUNA_E2E_CREDENTIAL_DIR"] = cred_dir
        # Where FaunaApp saves e2e-mode downloads (SnapshotFileSaver.e2eDownloadDir,
        # apple's twin of windows' DirectorySnapshotFileSaver): a caller override
        # via config["environment"] (already merged into `env` above) wins, else a
        # fresh per-launch tmp subdir — an explicit env var rather than a Swift-side
        # default so the harness names the dir it will read back.
        #
        # Pre-launch at-rest seeding (`seed_app_support`) — the "this install
        # existed before the upgrade" fixture: {relative path under Application
        # Support: local file to copy}. The seed lands under this launch's isolated
        # Application Support (set unconditionally above).
        self._seed_app_support(config)
        download_dir = env.get("FAUNA_E2E_DOWNLOAD_DIR") or os.path.join(tmp, "downloads")
        os.makedirs(download_dir, exist_ok=True)
        env["FAUNA_E2E_DOWNLOAD_DIR"] = download_dir
        self._download_dir = download_dir
        # The home-screen widget snapshot (`WidgetUnreadPublisher`): a
        # harness-named dir, same shape as the download dir above. A caller
        # override wins.
        widget_dir = env.get("FAUNA_E2E_WIDGET_DIR") or os.path.join(tmp, "widget")
        os.makedirs(widget_dir, exist_ok=True)
        env["FAUNA_E2E_WIDGET_DIR"] = widget_dir
        self._widget_dir = widget_dir
        # Pass the nest URL through so the agent's state protocol / onboarding can
        # reach it (parity with the other drivers' config["url"]).
        if config.get("url"):
            env["FAUNA_E2E_NEST_URL"] = config["url"]

        cmd = [str(exe), *shlex.split(config.get("app_args", "") or "")]
        self._launch_env = env

        if launch_mode == "photo-library":
            self._launch_through_launch_services(Path(self._bundle_path), env, config, tmp)
            return

        # Log to files (not PIPE) so a chatty app can't deadlock on a full pipe.
        self._app_stdout = open(os.path.join(tmp, "app.out"), "w")
        self._app_stderr = open(os.path.join(tmp, "app.err"), "w")
        # `popen_group_kwargs()` rather than a bare `start_new_session=True` (which
        # this was until 2026-08-14). That swap was cosmetic on macOS at the time —
        # identical setsid, but the one shared spelling, so the structural pin could
        # see this site at all — and it stopped being cosmetic on 2026-08-22, when
        # the shared call grew darwin's actual kernel half: the child's new pgid is
        # now registered with the run's pipe-EOF reaper from inside the preexec hook.
        # Before that this app was among the measured orphans (4h31m past its
        # pytest). `track_process` stays — it is the ORDERLY-exit teardown; the
        # kwargs are the guarantee that holds when no cleanup code runs.
        self._app_proc = subprocess.Popen(
            cmd,
            env=env,
            stdout=self._app_stdout,
            stderr=self._app_stderr,
            **popen_group_kwargs(),
        )
        track_process(self._app_proc)

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self._app_proc.poll() is not None:
                err = Path(os.path.join(tmp, "app.err")).read_text(errors="replace")
                raise RuntimeError(
                    f"FaunaMacOS exited early (rc={self._app_proc.returncode}). "
                    f"stderr tail: {err[-800:]}"
                )
            if self._agent_health_ok():
                self._bridge_dead = False
                return
            time.sleep(0.3)
        raise RuntimeError(f"in-process agent never became healthy on {self._url}")

    def relaunch_through_launch_services(self) -> None:
        """Bring a QUIT artifact bundle back the way a user does: `open` on the `.app`.

        The driver's own `launch()` execs the bundle's inner executable (it needs the
        per-instance env and PID ownership), which never touches LaunchServices'
        relaunch path — the door a macOS user takes back to the app after quitting
        it (`account-scoping.md` § Concurrent instances). This is that door, with the
        first launch's environment handed through `open --env` so the new process is
        the same isolated install (same HOME, credential store, agent port) and so
        signs back in as the same account from its own store.

        Refuses when the previous process is still running: `open` would then be the
        raise of a running instance (a different promise), not a relaunch. Adopts the
        new process as `_app_proc`, so `teardown()` ends it like any other launch.
        """
        if self._launch_mode != "bundle" or not self._bundle_path or not self._launch_env:
            raise RuntimeError(
                "a LaunchServices relaunch needs a bundle-mode launch to relaunch "
                f"(launch_mode={getattr(self, '_launch_mode', None)!r})"
            )
        if self.is_app_alive():
            raise RuntimeError(
                "the app is still running: quit it first, or `open` raises the "
                "running instance instead of relaunching"
            )
        old = self._app_proc
        for fp in (self._app_stdout, self._app_stderr):
            try:
                fp and fp.close()
            except Exception:
                pass
        if old is not None:
            untrack_process(old)
        self._launch_through_launch_services(
            Path(self._bundle_path), self._launch_env, self._launch_config,
            self._tmp_dir, fresh=False,
        )

    def _launch_through_launch_services(self, app: Path, env: dict, config: dict,
                                        tmp: str, *, fresh: bool = True) -> None:
        """Start a staged bundle with `open`, not Popen.

        Why `open` at all: see the photo-library venue section at the top of this
        module — it is what makes the app its own TCC-responsible process. `open`
        hands the app launchd's environment plus exactly the `--env` pairs given, so
        this passes everything the launch set or changed (the config's environment
        and the driver's own HOME / CFFIXED_USER_HOME / FAUNA_E2E_* keys) and the
        run's inherited FAUNA_* / RUST_* knobs; nothing else of the pytest process's
        environment is the app's business.

        `fresh` (`open -n -F`: a new instance, saved state ignored) is the venue's
        launch; `fresh=False` is a plain `open`, i.e. what a user's relaunch does
        (`relaunch_through_launch_services()`).
        """
        wanted = {k: v for k, v in env.items() if os.environ.get(k) != v}
        wanted.update({k: v for k, v in os.environ.items()
                       if k.startswith(("FAUNA_", "RUST_")) and k not in wanted})
        out_path = os.path.join(tmp, "app.out")
        err_path = os.path.join(tmp, "app.err")
        cmd = ["open", *(["-n", "-F"] if fresh else []),
               "--stdout", out_path, "--stderr", err_path]
        for key, value in sorted(wanted.items()):
            cmd += ["--env", f"{key}={value}"]
        cmd.append(str(app))
        args = shlex.split(config.get("app_args", "") or "")
        if args:
            cmd += ["--args", *args]
        self._app_stdout = None
        self._app_stderr = None
        opened = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        if opened.returncode != 0:
            raise RuntimeError(
                f"`open` refused the photo-library test bundle {app} "
                f"(rc={opened.returncode}): {(opened.stderr or opened.stdout).strip()[-600:]}"
            )

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self._agent_health_ok():
                break
            time.sleep(0.3)
        else:
            err = Path(err_path).read_text(errors="replace") if os.path.exists(err_path) else ""
            raise RuntimeError(
                f"the photo-library test bundle never served its in-process agent on "
                f"{self._url} after `open`. stderr tail: {err[-800:]}"
            )
        pid = _pid_listening_on(self._agent_port)
        if pid is None or not _pid_runs_bundle(pid, app):
            raise RuntimeError(
                f"the agent on port {self._agent_port} answers, but its listener "
                f"(pid {pid!r}) is not the staged bundle {app} — refusing to adopt a "
                "process this launch did not start"
            )
        self._app_proc = _LaunchedApp(pid)
        track_process(self._app_proc)
        if self._launch_mode == "photo-library":
            _PHOTO_LIBRARY_PIDFILE.write_text(str(pid))
        self._bridge_dead = False

    # ------------------------------------------------------------------
    # The photo-library venue's fixture seams (real_session only)
    # ------------------------------------------------------------------
    def _require_photo_library_venue(self, what: str) -> None:
        """Refuse a photo-library fixture outside the venue — convention 10.

        Every ordinary macOS launch is isolated from the box, but PhotoKit is not:
        a seed from a default launch would write a real photo into the host's real
        library. Only the real-session `photo-library` launch may.
        """
        if getattr(self, "_launch_mode", None) != "photo-library":
            raise RuntimeError(
                f"{what} on macOS reaches the machine-global System Photo Library, so "
                "it runs only under the real-session photo-library venue "
                "(tests/real_session/, the `real_photos_library` marker — convention 12); "
                f"this launch is {getattr(self, '_launch_mode', None)!r}"
            )

    def grant_photos_access(self) -> None:
        """The iOS twin writes the Photos grant into a throwaway device; macOS has no
        such store to write (the user TCC db is SIP-protected), so here the grant is
        the venue's one-time human "Allow" given to `PHOTO_LIBRARY_BUNDLE_ID` — and
        this only confirms the launch is the venue. `helpers.photo_backup.granted`
        still asserts the grant really took, from the app's own funnel."""
        self._require_photo_library_venue("grant_photos_access()")

    def request_photos_access(self, timeout: float) -> str:
        """Ask the OS for Photos access through the app and return its answer
        (`fauna_e2e_agent::PHOTO_BACKUP_REQUEST_ACCESS`). Undecided, this raises the
        real system alert and waits up to `timeout` for a human to click it."""
        self._require_photo_library_venue("request_photos_access()")
        reply = self.call_command("photo_backup_request_access", timeout=timeout)
        return json.loads(reply or "{}").get("authorization", "")

    def add_photo_to_library(self, host_path: str) -> None:
        """Put the image at `host_path` into the real System Photo Library — the
        venue's stand-in for "a photo was taken"
        (`fauna_e2e_agent::PHOTO_BACKUP_SEED_LIBRARY`), and PROVE it landed: the
        app fetches the new asset back before it answers, and an answer without an
        identifier is a fixture failure named here rather than a lost upload later.
        """
        self._require_photo_library_venue("add_photo_to_library()")
        reply = self.call_command("photo_backup_seed_library", {"path": host_path},
                                  timeout=60)
        seeded = json.loads(reply or "{}")
        if not seeded.get("local_identifier"):
            # A refusal rides the app's nav-independent refusal slot
            # (`AppMessages.errorForDisplay`), published as the state's `error` —
            # read there rather than off `error-message`, which no page renders
            # before sign-in (`helpers/agent_refusal.py`).
            refusal = ""
            try:
                refusal = (self._get("/app/state").get("state") or {}).get("error") or ""
            except Exception:
                pass
            # Before sign-in the snapshot carries no `error` at all; the refusal is
            # still logged loudly (`[TestAgent] test agent refused command: …`).
            if not refusal:
                marker = "test agent refused command:"
                lines = [l for l in self.app_stderr_text().splitlines() if marker in l]
                if lines:
                    refusal = lines[-1].split(marker, 1)[1].strip()
            raise RuntimeError(
                f"photo_backup_seed_library answered without an asset for {host_path} "
                f"(reply {reply!r}); the app's refusal: {refusal!r}"
            )

    def app_stderr_text(self) -> str:
        """This launch's `app.err` so far ("" if it has none yet).

        `logMessage(...)` calls forward through the UniFFI binding into the same
        shared `fauna_log` the Rust side uses, which writes every event to BOTH
        the daily-rolling file AND stderr — so this captures Swift-side
        `logMessage` lines (e.g. the `[sync-agent]` provisioning/unprovision
        lines) exactly like linux's/tui's twin of this method, without needing
        to locate the app's own log directory under its per-launch `home`.
        """
        tmp = getattr(self, "_tmp_dir", None)
        if not tmp:
            return ""
        try:
            return Path(os.path.join(tmp, "app.err")).read_text(errors="replace")
        except OSError:
            return ""

    def teardown(self) -> None:
        """Kill the app **and its whole process group**, then close the log files.

        `terminate_tree`, never a hand-rolled `killpg`. The app spawns
        `fauna-sync-agent` as a group child (`FfiChildAgentSpawner`), and that
        agent is BUILT to outlive the app that started it (`sync-agent.md`
        § Packaging + lifecycle) — right in production, an orphan in a test, and
        one that mass-deletes the user's live files when tmp GC empties the
        folder it still has bound (34 deletes propagated to example.com,
        2026-07-24).

        The hand-rolled teardown this replaces leaked the agent two independent
        ways: it computed `os.getpgid(proc.pid)` *inside* the `except` that
        swallows `ProcessLookupError`, so once the app had exited and been
        reaped (the health loop's own `poll()` does that) **no signal was sent
        at all**; and it escalated with a single-process `proc.kill()` rather
        than a group SIGKILL sweep, so a grandchild ignoring SIGTERM survived
        too. Regression proofs: `tests/test_harness_self_termination.py` § 3d.
        """
        proc = getattr(self, "_app_proc", None)
        if proc is not None:
            terminate_tree(proc)
            untrack_process(proc)
            self._app_proc = None
            if isinstance(proc, _LaunchedApp) and self._launch_mode == "photo-library":
                # The photo-library venue's leftover marker (`_sweep_leftover_…`):
                # its app is gone, so there is nothing for the next launch to end.
                try:
                    _PHOTO_LIBRARY_PIDFILE.unlink()
                except OSError:
                    pass
        for fp in (getattr(self, "_app_stdout", None), getattr(self, "_app_stderr", None)):
            try:
                fp and fp.close()
            except Exception:
                pass
