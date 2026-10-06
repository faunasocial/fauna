from __future__ import annotations

import os
import re
import shlex
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path

from .inprocess_agent import InProcessAgentDriver
from .port_util import find_free_port

BUNDLE_ID = "social.fauna.fauna"

# Every `xcrun simctl` call in the launch path is BOUNDED — e2e-conventions.md
# convention 9's "bounded always, unbounded never". These were unbounded until
# 2026-08-03, when `simctl terminate` wedged forever against a freshly-created
# ephemeral simulator and only pytest-timeout's 900s per-test kill ended it: the
# `app` fixture's per-module cold relaunch (convention 10) reported as ERROR at
# setup, 900s spent, with the traceback pointing at `selectors.poll` rather than
# at simctl — a hang the harness is supposed to be incapable of.
#
# Generous ceilings, NOT tuned latencies (convention 14): a healthy call answers
# in well under a second, but these run while the box is saturated (3-15
# parallel checkouts building at once is normal here) and while the ephemeral sim may still be
# booting. A green run pays nothing — the call returns as soon as simctl does.
_SIMCTL_CONTROL_S = 120.0
"""`terminate` / `uninstall` — control-plane pokes at an app that may not even
be installed (both are best-effort, `check=False`)."""

_SIMCTL_INSTALL_S = 300.0
"""`install` — copies the built .app into the simulator's container."""

_SIMCTL_BOOT_S = 300.0
"""`boot` / `launch` — the two calls that wait on the simulator's own runtime
to come up, the slowest legitimate waits in this driver."""

_SIMCTL_BOOTSTATUS_S = 900.0
"""`bootstatus -b` — waiting for a device to FINISH booting, which is a
different and much longer wait than `boot` returning (`_ensure_booted`'s
docstring has the measurement: 1.1 s to `Booted`, 513 s to finished). The
ceiling is a wedge-detector, not a tuned latency, and it may exceed the per-test
budget precisely because `conftest._prebuild_binaries` hoists this wait out of
every test's clock."""


def _run_simctl(
    args: list[str], *, timeout: float, what: str, best_effort: bool = False, **kwargs
):
    """`subprocess.run` for simctl with a hard ceiling and a self-diagnosing
    failure. A timeout here is a WEDGED simulator, never a slow one — say so,
    naming the sub-command, rather than letting the per-test killer report an
    anonymous `selectors.poll`.

    `best_effort=True` marks a call whose FAILURE is already tolerated
    (`terminate` / `uninstall` — "make sure it isn't running / isn't
    installed", both no-ops on a fresh simulator where the app was never
    there). Those must not be fatal on timeout either: `simctl terminate` has
    been observed wedging for >120s against a healthy, Booted sim while the box
    is saturated, and failing the launch on a cleanup poke that had nothing to
    clean would be strictly worse than proceeding — the `install`/`launch` that
    follow are fatal and will surface any real problem. Loud, never silent
    (convention 11): the timeout is reported, it just doesn't abort.
    """
    kwargs.setdefault("check", False)
    kwargs.setdefault("capture_output", True)
    kwargs.setdefault("text", True)
    try:
        return subprocess.run(args, timeout=timeout, **kwargs)
    except subprocess.TimeoutExpired:
        if best_effort:
            print(
                f"[ios-driver] WARNING: `xcrun simctl {what}` did not return "
                f"within {timeout}s; continuing, because this call is "
                f"best-effort cleanup. If the launch below fails, the first "
                f"thing to suspect is a device that had not FINISHED booting "
                f"when this ran — see `_ensure_booted`.",
                flush=True,
            )
            return subprocess.CompletedProcess(args, returncode=124, stdout="", stderr="")
        raise RuntimeError(
            f"`xcrun simctl {what}` did not return within {timeout}s.\n"
            f"  command: {' '.join(args)}\n"
            f"The overwhelmingly likely cause is a device that is Booted but has "
            f"not finished booting — its app-layer services (installd, "
            f"FrontBoard) are not answering yet, and `terminate`/`uninstall`/"
            f"`install` all block on them. `_ensure_booted` is supposed to have "
            f"closed that window with `simctl bootstatus -b`; if you are here, "
            f"check that it ran for THIS device (measured 2026-09-22: 1.1 s to "
            f"`Booted`, 513 s to finished). Only if the device really is wedged "
            f"is `xcrun simctl shutdown <udid>` the cure — never `shutdown all`, "
            f"which would drop a sibling session's simulator."
        ) from None


def boot_and_wait_until_usable(udid: str) -> None:
    """Boot `udid` and block until it has FINISHED booting — the ONE spelling of
    "this simulator is usable", shared by the driver's own `_ensure_booted` and
    by `conftest._get_ios_setup`, which runs it at collection time so the wait is
    never charged to a test's clock.

    Two callers rather than two copies on purpose: the whole defect this closes
    was a *second*, weaker idea of "booted" (polling `simctl list devices` for
    the word) living beside the real one. A device that only satisfies the weak
    test is one every app-layer `simctl` call then blocks on — see
    `IosInProcessDriver._ensure_booted` for the measurement.

    Idempotent: `boot` on an already-booted device is a no-op error this ignores
    (it is `check=False`), and `bootstatus -b` on a finished device returns at
    once, which is what makes the collection-time call and the per-launch call
    compose without the launch paying twice.
    """
    _run_simctl(
        ["xcrun", "simctl", "boot", udid],
        timeout=_SIMCTL_BOOT_S, what="boot",
    )
    status = _run_simctl(
        ["xcrun", "simctl", "bootstatus", udid, "-b"],
        timeout=_SIMCTL_BOOTSTATUS_S, what="bootstatus",
    )
    if status.returncode != 0:
        raise RuntimeError(
            f"iOS simulator {udid} never finished booting: `simctl bootstatus -b` "
            f"exited {status.returncode}. Every later simctl call against it "
            f"(terminate, install, launch) would block on, or be refused by, a "
            f"device whose app-layer services are not up — so this fails here "
            f"instead of ~9 minutes later with an anonymous timeout.\n"
            f"  stdout: {(status.stdout or '').strip() or '<empty>'}\n"
            f"  stderr: {(status.stderr or '').strip() or '<empty>'}"
        )


# ---------------------------------------------------------------------------
# In-process automation agent (the sole iOS e2e driver) — the former XCUITest
# bridge (`AppleBridgeDriver`) + apple-bridge runner were retired at cutover.
# ---------------------------------------------------------------------------
# The iOS app hosts the same `InProcessAutomationServer` + `AutomationRegistry`
# the macOS app does (shared FaunaKit), bound to a per-instance
# FAUNA_E2E_AGENT_PORT — the exact in-process shape linux + macOS already use.
# This driver runs the app in the iOS Simulator via `simctl` (the sim shares the
# host's loopback, so the app's `127.0.0.1:port` server is reachable from the
# host at the same address) and talks straight to its `/element/*` + `/app/*`
# surface. NO xcodebuild test-runner, NO XCUITest, NO machine-wide AutomationMode
# → nothing to wedge (no reboots).
#
# Authority + Phase-0/3 findings: docs/goal/architecture/apps/apple-e2e-automation.md.
# The sole iOS e2e driver as of the XCUITest retirement cutover (Phase 3 reached
# in-process parity with the iOS XCUITest baseline, so the bridge was deleted).
#
# The post-click settle, the `/health` probe and `recover()` are shared with the
# macOS in-process driver via `InProcessAgentDriver`; only the process model (a
# `simctl` install+launch+terminate, vs macOS's direct `FaunaMacOS` Popen) is
# iOS-specific and lives here.


class IosInProcessDriver(InProcessAgentDriver):
    """iOS E2E driver backed by the in-process automation server.

    `launch()` installs + launches the app in the booted Simulator with a
    per-instance FAUNA_E2E_AGENT_PORT (passed through `simctl`'s
    `SIMCTL_CHILD_*` env forwarding) and drives it over its in-app HTTP server.
    `teardown()` terminates it; the inherited `recover()` relaunches a wedged one.

    Note on concurrency: unlike macOS (a fresh isolated HOME per launch), the iOS
    app's on-disk container is per-(simulator, bundle-id). Cross-session
    concurrency is solved upstream in conftest `_create_ephemeral_simulator`,
    which gives each pytest *session* its own throwaway `simctl`-created device —
    the iOS analogue of linux/macOS's per-launch private state dir (the device is
    the unit of on-disk isolation, since an iOS app can't run as a bare process).
    Within a session, a clean container per launch is ensured by an
    uninstall+install in `launch()` — except for a launch a test explicitly
    pinned via `preserve_state_across_relaunch()`, which skips the uninstall so
    the container's at-rest state survives (see that method).
    Two iOS apps launched *simultaneously in one session* would share this
    device by default (`launch()`'s own uninstall+install tearing down
    whichever seat launched first) — closed for the two-seat second-seat
    fixtures (`caldav_mailbox_less_attendee_app`,
    `_launch_second_real_faunamls_app`, row 290) via conftest's
    `_get_ios_second_seat_udid`, a second session-scoped ephemeral device
    those fixtures hand this driver's `config["udid"]` instead of
    `_get_ios_setup`'s. A THIRD concurrently-live seat and beyond (the
    room-model journeys seat three) takes its own device from conftest's
    `_get_ios_seat_udid`. The keychain is in-memory under
    FAUNA_E2E_AGENT_PORT (FaunaKit's `KeychainStore` E2E gate), so the real
    sim keychain is never touched.
    """

    BUNDLE_ID = BUNDLE_ID

    # Same real POST /element/scroll-into-view as macOS — the route lives in the
    # SHARED FaunaKit automation server, so both apple apps gained it in one
    # change (see `MacosInProcessDriver` for the note). On iOS the walk finds the
    # UIScrollView ancestors; the sentinel-based walk is what makes a presented
    # sheet scroll its OWN scroll view rather than the one behind it.
    _supports_scroll_into_view = True

    #: The at-rest twin of `_preserved_cred_dir`, and the iOS counterpart of
    #: `MacosInProcessDriver._preserved_home`: when True, `launch()` SKIPS the
    #: uninstall so the app's data container — and with it `Library/Application
    #: Support` — survives the relaunch. macOS gets this for free by pinning the
    #: relocated HOME; on iOS the at-rest state lives inside the container the
    #: uninstall throws away, so preserving it has to be an explicit branch.
    #: Without it `preserve_state_across_relaunch()` would return True while
    #: delivering only HALF the durability contract (keychain yes, Application
    #: Support no) — which is exactly how a nest-identity pin, an MLS db, or a
    #: SwiftData store silently fails to survive a relaunch the test believes it
    #: preserved.
    _preserve_container: bool = False
    #: Opt-in (`grant_photos_access()`): pre-grant the app's Photos TCC entry on
    #: the throwaway simulator device, so `PHPhotoLibrary.requestAuthorization`
    #: resolves `.authorized` with no visible system prompt. Honored at the NEXT
    #: launch, after `simctl install` and before `simctl launch` — the app must
    #: exist for `simctl privacy` to name it, and must not be running when its
    #: TCC entry changes (the grant restarts a running app).
    _grant_photos: bool = False
    #: The device this driver's CURRENT app process runs on, kept so a test can
    #: reach the simulator after launch (`add_photo_to_library`). `launch()` is
    #: the only writer.
    _udid: str | None = None
    #: Where the CURRENT app process saves e2e-mode downloads (see `launch`
    #: and `download_dir()`); the sim shares the host filesystem, so this is
    #: a normal host path the test process can read back directly.
    _download_dir: str | None = None
    _widget_dir: str | None = None
    #: The real host PID of the CURRENT app process, parsed from `simctl launch`'s
    #: own stdout (`"<bundle-id>: <pid>"`) — a Simulator app is a genuine host
    #: process (verified empirically: `ps -p <pid>` resolves it, and `kill -9` on it
    #: terminates cleanly), so this is what makes `kill_uncleanly()` possible despite
    #: `simctl` (not this driver) owning the Popen.
    _app_pid: int | None = None
    #: This launch's `Library/Application Support` INSIDE the simulator's app data
    #: container (see `app_support_dir()`).
    _app_support: str | None = None

    #: `{log file path: bytes already present when THIS launch installed}` — the
    #: per-launch floor `app_log_text` reads from, so "the app's own words" never
    #: means a DEAD predecessor's.
    #:
    #: Normally empty: `launch()` uninstalls the data container, so the app starts
    #: with no `logs/` at all. It is non-empty exactly when
    #: `preserve_state_across_relaunch()` pinned the container — and that pin
    #: survives into the module-boundary relaunch, because `module_relaunch`
    #: runs BEFORE the `driver.reset()` that clears it. Without this floor, a
    #: reader asking "did the app log X this launch" would be answered by the
    #: previous launch's file, which is how a control that must not be
    #: satisfiable by anything but the real thing becomes satisfiable by a
    #: ghost.
    #:
    #: Same shape as windows' `_app_log_size`/`_app_log_since` byte mark, which
    #: exists for the same reason (its data-dir log is append-shared across
    #: relaunches) — one spelling of "the app's words SINCE this launch".
    _log_baseline: dict[str, int] | None = None

    def log_scope_across_relaunch(self) -> str:
        """``"per-launch"`` — `launch()` uninstalls the container (taking `logs/` with it), and under a `preserve_state_across_relaunch()` pin `_mark_log_baseline()` takes a byte floor before the app starts.

        See the base declaration for what each answer means and why it is
        declared rather than inferred; pinned per driver by
        `tests/test_module_relaunch.py`.
        """
        return "per-launch"

    def is_mobile(self) -> bool:
        return True

    def app_support_dir(self) -> str | None:
        """This launch's ``Library/Application Support`` — where the app's
        account-scoped stores land (``Fauna/<actor-id-hex>/mls.db``;
        `account-scoping.md` § Serialized switching — ``segment-backup`` was
        one such store too until the in-app coordinator was deleted fleet-wide
        2026-08-15, `backup-restore.md` § Background Tasks → *Flip status
        (slice 5)*). The macOS twin returns the relocated-HOME path; here it is the
        simulator's app data container, which the sim shares with the host
        filesystem, so it is directly readable by the test process (same reasoning
        as `_cred_dir` / `download_dir()`).

        Resolved in `launch()` right after the install, so it is the container the
        CURRENT process runs in — the uninstall+install there means the previous
        launch's path is stale by construction.
        """
        return self._app_support

    @property
    def sync_agent_state_base(self) -> str | None:
        """Where THIS launch keeps this device's sealed custodian store, flat:
        `<container>/Library/Application Support/Fauna` — `AccountStateDir.base`,
        under which shared Rust scopes `<actor-id-hex>/backup-custodian` itself
        (`fauna_sync_engine::custodian_store::custodian_store_root`).

        The name is the cross-driver one (`helpers/sync_agent_config.py` reads it on
        every app), not a claim that iOS runs an agent: it has none. The in-app
        custodian host (`CustodianBackupEngine`) plays the agent's part and keeps
        its store under the UNSCOPED per-user base — deliberately NOT the
        `<Application Support>/Fauna/sync` root `_resolved_store_root` publishes,
        which is the *account* store's, so answering that here would search a
        directory the custodian never writes to.

        ``None`` when the container could not be located (see `_resolve_app_support`).
        """
        return os.path.join(self._app_support, "Fauna") if self._app_support else None

    def _resolve_app_support(self, udid: str) -> None:
        """Locate the freshly-installed app's data container and record its
        Application Support dir. Best-effort: a failure leaves `None`, which reads
        the same way as macOS's un-relocated launch — a caller can never mistake it
        for a real path."""
        out = _run_simctl(
            ["xcrun", "simctl", "get_app_container", udid, self.BUNDLE_ID, "data"],
            timeout=_SIMCTL_CONTROL_S, what="get_app_container",
        )
        if out.returncode != 0:
            self._app_support = None
            return
        container = out.stdout.strip()
        self._app_support = (
            str(Path(container) / "Library" / "Application Support") if container else None
        )

    def _mark_log_baseline(self) -> None:
        """Record how much log each file already holds, before the app starts.

        Called between `simctl install` and `simctl launch`, the one window in
        which the container exists and the app is not yet running — so every byte
        counted here belongs to a PREVIOUS launch by construction, and no race
        with the live app is possible.
        """
        self._log_baseline = {}
        if not self._app_support:
            return
        for path in Path(self._app_support).glob("logs/*"):
            try:
                self._log_baseline[str(path)] = path.stat().st_size
            except OSError:  # a file we cannot stat is a floor of zero, not a crash
                continue

    def _seed_app_support(self, config: dict) -> None:
        """Copy ``config["seed_app_support"]`` — ``{relpath: local file}`` — into the
        freshly-installed container, before the app is launched. The iOS twin of
        `MacosInProcessDriver._seed_app_support`; see that docstring for what the
        seam is for."""
        seed = config.get("seed_app_support")
        if not seed or not self._app_support:
            return
        for rel, source in seed.items():
            target = Path(self._app_support) / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)

    def is_ios(self) -> bool:
        return True

    def enter_background(self) -> None:
        """Send the app to the background — the real
        `UIApplicationDelegate.applicationDidEnterBackground`, invoked on the
        real delegate by the in-process automation server.

        This is iOS's leave door (`reserved-folders.md` § The leave-flush
        promise: "moving to the background on a phone — the OS may kill a
        backgrounded app at any moment, so the flush rides the background
        transition, never the kill"). macOS's is a real quit (`quit_app`),
        linux's and windows' their window close, tui's its `exit-tab`, web's
        `pagehide`.

        The app keeps RUNNING afterwards — backgrounding is not an exit — so
        assert the outcome by what the transition was meant to produce (e.g. the
        nest-side drafts blob), never by process death. `simctl` offers no verb
        that backgrounds an app, which is why this drives the delegate callback
        in-process rather than from outside.
        """
        self._post("/app/lifecycle/background", {})

    def reset(self, timeout: float = 10.0) -> None:
        """Between-tests reset. Also DROPS any credential-dir pin — mirrors
        `MacosInProcessDriver.reset` exactly (this driver is session-scoped via
        conftest `_driver_cache` too, so a pin from one test must not leak into
        the next test's `recover()`/`hard_reload()`)."""
        self._preserved_cred_dir = None
        self._preserve_container = False
        self._grant_photos = False
        super().reset(timeout=timeout)

    def supports_unclean_kill(self) -> bool:
        return self._app_pid is not None

    def kill_uncleanly(self) -> None:
        """SIGKILL the CURRENT launch's real host process (see
        `PlatformDriver.kill_uncleanly` for the crash-simulation contract).

        Only ever signals `self._app_pid` — the PID `simctl launch` reported for
        THIS driver's own most recent launch, never a name/bundle-id-based kill —
        so this stays within the process-safety rule (e2e-unified/README.md) even
        though `simctl`, not this driver, is nominally the process's parent.
        """
        if self._app_pid is None:
            super().kill_uncleanly()  # the NotImplementedError with the contract
            return
        try:
            os.kill(self._app_pid, signal.SIGKILL)
        except ProcessLookupError:
            pass  # already dead
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            try:
                os.kill(self._app_pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.1)
        self._app_pid = None

    def preserve_state_across_relaunch(self) -> bool:
        """Pin this launch's E2E keychain dir **and its app data container** so a
        later `recover()` / `hard_reload()` relaunch reads back what this process
        wrote — the iOS twin of
        `MacosInProcessDriver.preserve_state_across_relaunch`. See the base
        contract; `launch()` below is what actually honours both pins.

        **Both halves are required, and the container half is the easy one to
        miss.** macOS satisfies the at-rest half by pinning the relocated HOME
        (`_preserved_home`), so its `Library/Application Support` persists for
        free. iOS's Application Support lives *inside* the simulator app data
        container, which `launch()` normally destroys via `simctl uninstall` —
        so without `_preserve_container` the keychain would survive while every
        at-rest store rooted in Application Support silently would not. The
        nest-identity pin store is exactly such a store (`FaunaKit`'s
        `NestTrust.installPinStore()` roots the E2E launch at
        `<Application Support>/Fauna`), which is what surfaced this: the
        identity-pin relaunch case seeded a pin, got `True` back from this
        method, and then found the pin gone — reading as a product bug when it
        was a half-kept harness promise.
        """
        if self._cred_dir is None:
            return False
        self._preserved_cred_dir = self._cred_dir
        self._preserve_container = True
        return True

    def grant_photos_access(self) -> None:
        """Pre-grant Photos (PhotoKit) access to the app, from the NEXT launch on.

        `PhotoBackupControlsView` gates its whole surface behind
        `PHPhotoLibrary.requestAuthorization`, which on a simulator raises a real
        system alert no in-process driver can reach — the automation server sees
        the app's own view tree, never SpringBoard's. `simctl privacy grant`
        writes the TCC entry directly, so the request resolves `.authorized`
        without a prompt and the walk drives the app's own controls.

        Deliberately opt-in rather than part of every launch (point 10 — a launch
        closes inherited channels by default). It is not a machine-global
        dependency either way: the device is a throwaway `simctl`-created one,
        per pytest session.
        """
        self._grant_photos = True

    #: TCC consent-model generation the iOS 26 runtime requires of a Photos row.
    #: See `_finish_photos_grant`.
    _TCC_PHOTOS_AUTH_VERSION = 2

    def _tcc_db(self, udid: str) -> Path:
        """The simulator device's own TCC store (an ordinary host file — the
        simulator shares the filesystem, same as `add_photo_to_library`'s
        `Media/DCIM`)."""
        return (
            Path.home() / "Library" / "Developer" / "CoreSimulator"
            / "Devices" / udid / "data" / "Library" / "TCC" / "TCC.db"
        )

    def _finish_photos_grant(self, udid: str) -> None:
        """Make the Photos grant `simctl privacy` just wrote actually take.

        **`simctl privacy grant photos` exits 0 and writes a correct-looking row
        that iOS 26 then ignores.** It stamps `auth_version = 1`; the iOS 26.5
        runtime's `tccd` reads that row, treats generation 1 as a stale consent,
        and prompts anyway — `PHPhotoLibrary.authorizationStatus` stays
        `notDetermined`, the SpringBoard alert nothing can answer eventually
        times out into a *denial* written back over the grant, and the app sees
        an empty library. Measured on macOS, iOS 26.5, paired control run twice:

            grant only            -> status=notDetermined assets=0
            grant + auth_version  -> status=authorized    assets=6

        and in `tccd`'s own log the grant-only arm reads
        `Got 1 auth from db ... flags: 0` immediately followed by
        `AUTHREQ_PROMPTING` — the row is found and then declined as authority.
        Bumping the single `auth_version` column is the whole fix: `auth_value`
        (2) and `auth_reason` (4) are already what a real consent carries, and
        re-signing the bundle so its code-signing identifier matches
        `CFBundleIdentifier` — the first hypothesis — changes nothing (`tccd`
        resolves the subject from LaunchServices; the log's `AUTHREQ_SUBJECT`
        names the bundle id even for the unsigned-identifier bundle
        `conftest._build_ios_app` produces).

        Patching rather than hand-writing the row keeps `simctl privacy` the
        author of every other column (`csreq`, the indirect-object fields), and
        makes this self-retiring: the day a runtime writes generation 2 itself
        the UPDATE matches nothing and the verification below still passes.

        Applied to the booted device with no reboot, immediately before
        `simctl launch` — verified in the same paired control.
        """
        db = self._tcc_db(udid)
        if not db.exists():
            raise RuntimeError(
                f"the simulator's TCC store is missing at {db} — the Photos "
                "grant cannot be completed, and without it PhotoKit stays "
                "notDetermined (see _finish_photos_grant)."
            )
        try:
            with sqlite3.connect(str(db), timeout=_SIMCTL_CONTROL_S) as conn:
                conn.execute(
                    "UPDATE access SET auth_version = ? "
                    "WHERE service = 'kTCCServicePhotos' AND client = ?",
                    (self._TCC_PHOTOS_AUTH_VERSION, self.BUNDLE_ID),
                )
                row = conn.execute(
                    "SELECT auth_value, auth_version FROM access "
                    "WHERE service = 'kTCCServicePhotos' AND client = ?",
                    (self.BUNDLE_ID,),
                ).fetchone()
        except sqlite3.Error as exc:
            raise RuntimeError(
                f"could not complete the Photos grant in {db}: {exc}. "
                "PhotoKit would come up notDetermined and every photo-backup "
                "pass would silently see an empty library "
                "(see _finish_photos_grant)."
            ) from exc

        # And PROVE it, exactly as the grant above and `add_photo_to_library` do
        # — this whole method exists because a step that fails invisibly here is
        # indistinguishable, from outside the app, from a real ingest bug.
        if row is None:
            raise RuntimeError(
                f"simctl privacy grant photos exited 0 but wrote no "
                f"kTCCServicePhotos row for {self.BUNDLE_ID} in {db}."
            )
        auth_value, auth_version = row
        if auth_value != 2 or auth_version != self._TCC_PHOTOS_AUTH_VERSION:
            raise RuntimeError(
                f"the Photos TCC row for {self.BUNDLE_ID} reads "
                f"auth_value={auth_value!r} auth_version={auth_version!r}, "
                f"wanted auth_value=2 "
                f"auth_version={self._TCC_PHOTOS_AUTH_VERSION}. PhotoKit will "
                "report notDetermined and the backup pass will see no assets "
                "(see _finish_photos_grant)."
            )

    def add_photo_to_library(self, host_path: str) -> None:
        """Put `host_path` (an image or video file) into the simulator's Photos
        library — the e2e stand-in for "a photo taken on the phone".

        The simulator shares the host filesystem, so an ordinary host path works.
        Requires a launched driver: the device is only known from `launch`.
        """
        if not self._udid:
            raise RuntimeError(
                "add_photo_to_library() before launch() — the simulator device "
                "is only known once the app has been launched on it."
            )
        result = _run_simctl(
            ["xcrun", "simctl", "addmedia", self._udid, host_path],
            timeout=_SIMCTL_CONTROL_S,
            what="addmedia",
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"simctl addmedia failed (rc={result.returncode}) for "
                f"{host_path!r}: {result.stderr.strip() or '<no stderr>'}"
            )
        # And PROVE it landed. `addmedia` exits 0 on a device whose photo
        # library never actually took the asset, which would surface much later
        # as "the app backed up nothing" — a fixture failure wearing a product
        # failure's clothes. The simulator's library is an ordinary host
        # directory, so the check is a stat, not a guess.
        media_root = (
            Path.home() / "Library" / "Developer" / "CoreSimulator"
            / "Devices" / self._udid / "data" / "Media" / "DCIM"
        )
        assets = (
            [q for q in media_root.rglob("*") if q.is_file()]
            if media_root.exists() else []
        )
        if not assets:
            raise RuntimeError(
                f"`simctl addmedia` reported success for {host_path!r} but the "
                f"simulator's photo library is empty — nothing under "
                f"{media_root} (exists={media_root.exists()}). The asset never "
                f"reached the device, so any test asserting on it would be "
                f"grading the fixture, not the app."
            )

    # ----------------------------------------------------------------------
    # Process lifecycle (iOS Simulator via simctl)
    # ----------------------------------------------------------------------
    def _ensure_booted(self, udid: str) -> None:
        """Block until `udid` has FINISHED booting, or FAIL — never fall through.

        The fall-through is what this method used to do: a fixed `for _ in
        range(30)` poll that simply ran out and returned, leaving `launch()` to
        run `simctl terminate` against a simulator that was never up. That call
        then blocked indefinitely, and the only thing that ended it was
        pytest-timeout's 900s per-test kill, whose traceback pointed at
        `selectors.poll` — so the visible symptom named neither the simulator
        nor the boot. Silently proceeding on an unmet precondition converts a
        diagnosable failure into a hang; assert the precondition instead
        (e2e-conventions.md convention 11 — an illegal state is refused loudly,
        never half-honoured).

        ⚠ **`simctl list devices` saying "Booted" is NOT the precondition — it
        is the start of the boot, not the end of it (measured 2026-09-22).** The
        deadline poll this replaced watched for that word and returned on it,
        which on a freshly created device is ~1 s in, while the device needs
        minutes more before its app-layer services (installd, FrontBoard) will
        answer. Measured on a freshly created ephemeral device on this box:
        `list devices` said `Booted` after **1.1 s**, and `simctl bootstatus -b`
        only returned after **513 s** — a 512-second window in which the device
        looks up and is not. `launch()` runs straight into that window, so
        `terminate` and `uninstall` burn their whole 120 s best-effort ceilings
        and `install` its 300 s fatal one (≈540 s, which is why the failure took
        ~9 minutes), or `launch` is refused outright with `Application
        "social.fauna.fauna" is unknown to FrontBoard`. Every test AFTER the
        first one then passes, because the boot finished while the first one was
        failing — which is exactly what made this read as a *wedged daemon* that
        mysteriously healed itself, and what `_run_simctl`'s own timeout message
        used to assert. It is neither wedged nor slow: it was never asked to
        finish.

        So the precondition is `simctl bootstatus <udid> -b`, which is the one
        call that waits for boot COMPLETION. Still bounded, on its own named
        ceiling (convention 14): a warm device returns at once and a green run
        pays nothing, but a cold first boot of a freshly created device on a
        saturated box legitimately costs several minutes, so the ceiling is a
        wedge-detector rather than a tuned latency — and the wait is hoisted out
        of any test's own clock by `conftest._prebuild_binaries`, which is what
        makes a ceiling this generous affordable (convention 9's rule that a
        build, and the wait in front of it, is never charged to a test).
        """
        boot_and_wait_until_usable(udid)

    def download_dir(self) -> str | None:
        """Where FaunaApp saves e2e-mode downloads (no save dialog under e2e).

        Mirrors `MacosInProcessDriver.download_dir()` and windows'
        `WindowsBridgeDriver.download_dir()`: set fresh every `launch()`
        (see `_download_dir`). The simulator shares the host filesystem, so
        the path is directly readable by the test process — no
        `simctl get_app_container` translation needed (same reasoning as
        `_cred_dir`).
        """
        return self._download_dir

    def widget_dir(self) -> str | None:
        """Where this launch's app writes the home-screen widget snapshot
        (`FAUNA_E2E_WIDGET_DIR`, fresh every `launch()`; host-readable, like
        `download_dir()`). Mirrors `MacosInProcessDriver.widget_dir()`.
        """
        return self._widget_dir

    #: The resource the shell's `CloudBackupExcluder` sets — on the simulator's
    #: host-shared filesystem it is this extended attribute. **Two encodings,
    #: both measured 2026-08-26** (a probe binary spawned inside a booted
    #: simulator via `simctl spawn`, xattrs read from the host): the iOS
    #: runtime's Foundation writes the value `com.apple.MobileBackup`, macOS's
    #: writes `com.apple.backupd` for the same key. The first run of this
    #: witness expected only the macOS value and read a correctly excluded dir
    #: as `False`.
    _BACKUP_EXCLUDE_XATTR = "com.apple.metadata:com_apple_backup_excludeItem"
    _BACKUP_EXCLUDE_VALUES = ("com.apple.MobileBackup", "com.apple.backupd")

    def account_store_cloud_backup_excluded(self, actor_id_hex: str) -> bool | None:
        """Whether this launch's W3 (account-data-plane.md § Workstreams) account-store dir for `actor_id_hex` is
        excluded from iCloud device backup — the on-disk witness of
        `AccountRuntimeParams::store_backup_exclusion`'s shell arm
        (`common.md` § Credential storage → *The shared Rust credential slots
        on the phones*: the writer key is a `ThisDeviceOnly` keychain row a
        restore does not carry, so the store dir must be out of the same
        backup or a restored phone strands its account plane).

        `None` when the dir does not exist yet (nothing assembled) or the
        container is unresolved; `True`/`False` otherwise. Read from the host
        as the extended attribute the shell's `URLResourceValues` write lands
        as: under e2e the store roots under `<app-support>/Fauna/sync`
        (`AccountStateDir.storeContainerDir`'s e2e branch), and the simulator
        shares the host filesystem, so no `simctl` translation is needed.
        """
        if not self._app_support:
            return None
        store_dir = (
            Path(self._app_support) / "Fauna" / "sync" / actor_id_hex.lower() / "account-store"
        )
        if not store_dir.is_dir():
            return None
        # The `xattr` CLI rather than `os.getxattr`: CPython exposes the latter
        # on Linux only, and this driver runs on the mac host. A missing
        # attribute is a non-zero exit — the dir exists but nothing excluded it.
        out = subprocess.run(
            ["xattr", "-p", self._BACKUP_EXCLUDE_XATTR, str(store_dir)],
            capture_output=True, text=True, timeout=_SIMCTL_CONTROL_S, check=False,
        )
        return out.returncode == 0 and out.stdout.strip() in self._BACKUP_EXCLUDE_VALUES

    def app_log_text(self) -> str | None:
        """This launch's on-disk app log — the client's own account of itself,
        attached to every failing test's report by `helpers/app_log_section.py`.

        The iOS twin of `WindowsBridgeDriver.app_log_text`, reading the same
        `<data-dir>/logs/fauna.log.<date>` the shared `fauna_log` writes on
        every native client (`libs/fauna-log/src/lib.rs::native::init`,
        `observability.md` § Shared capture). `FaunaApp.init()` passes
        `.applicationSupportDirectory` to `installLogging`, and on iOS that
        resolves inside the app's own data container — which is exactly
        `_app_support`, already resolved by `launch()` after the install. The
        simulator shares the host filesystem, so the path the app writes is the
        path this process reads, with no `simctl` translation (same reasoning as
        `_cred_dir` / `download_dir()`).

        **This driver captured NOTHING before 2026-08-26, and it cost a
        diagnosis.** The account-runtime acceptance test's own failure message
        routes the reader to "grep the app log for `account runtime:`" to
        separate *assembly failed* from *superseded* from *refused at the door*
        from *the hook never fired* — and on iOS
        there was nothing to grep, so two ~12-minute runs produced the same
        unreadable `(False, False)` and the track stalled. macOS and windows had
        their log surfaces; iOS was the family-wide hole.

        **Why the rolling file and not `simctl spawn <udid> log stream`.** The
        app's own durable record already exists inside a host-readable
        container, so a streaming capture would add a second process and a live
        pipe that must be drained for as long as the child lives (convention 13)
        to obtain *less*: `log stream` sees os_log, while `fauna_log`'s tracing
        output reaches os_log only incidentally through launchd_sim's stdio
        forwarding. Reading the file also works retroactively — the container
        outlives the app process and is reclaimed only by the NEXT launch's
        `simctl uninstall` or the session's ephemeral-device teardown, both of
        which happen after `makereport(when="call")` has already read it.

        `None` (no dir / no files) is a deliberate answer, not silence: it makes
        `app_log_section` emit its explicit `<empty>` section, which is itself
        the verdict "the app never started or never logged" — distinct from the
        no-section-at-all a driver family with no log gets.
        """
        if not self._app_support:
            return None
        baseline = self._log_baseline or {}
        chunks = []
        for path in sorted(Path(self._app_support).glob("logs/*")):
            # Slice from this launch's floor rather than returning the whole
            # file: under a `preserve_state_across_relaunch()` pin the container
            # — and so `logs/` — carries over, and a reader that returned the
            # predecessor's bytes would answer a different question than the one
            # asked.
            floor = baseline.get(str(path), 0)
            try:
                with open(path, "rb") as fh:
                    if floor:
                        fh.seek(floor)
                    text = fh.read().decode("utf-8", errors="replace")
            except OSError:
                continue
            if text:
                chunks.append(text)
        return "\n".join(chunks) if chunks else None

    def app_stderr_text(self) -> str:
        """This launch's on-disk app log, `""` if it has none yet.

        The cross-app `app_stderr_text()` contract (linux/tui/macos/windows) —
        `""` for "nothing captured", never `None` (`_printed_events()` and
        `helpers/folder_content.py`'s readers call `.splitlines()`/index into
        it directly). `app_log_text()` already reads the exact same rolling
        `fauna.log.<date>` file the shared `fauna_log` fmt layer writes
        (macOS's `app_stderr_text()` twin, same tracing-line format since both
        platforms call `installLogging` — `observability.md` § Shared
        capture); this only adapts its `None`-on-empty answer to the `""`
        every other driver's `app_stderr_text()` gives.
        """
        return self.app_log_text() or ""

    def launch(self, config: dict) -> None:
        self._launch_config = config
        udid = config.get("udid")
        app_path = config.get("app_path")
        if not udid or not app_path:
            raise RuntimeError(
                "iOS in-process launch needs both 'udid' and 'app_path' in the "
                "config (supplied by conftest._get_ios_setup)."
            )
        if not Path(app_path).exists():
            raise RuntimeError(
                f"iOS app bundle not found at {app_path}.\n"
                f"Run 'just apple-ffi-test' then rebuild via the iOS e2e setup."
            )

        port = find_free_port()
        self._agent_port = port
        self._url = f"http://127.0.0.1:{port}"

        self._ensure_booted(udid)
        self._udid = udid

        # Clean container per session: terminate any stale instance, then
        # uninstall+install so the on-disk SwiftData store starts empty (the iOS
        # analogue of macOS's fresh-HOME isolation). Best-effort on terminate /
        # uninstall (no app installed yet on the first run is fine).
        #
        # `_preserve_container` (a test's explicit
        # `preserve_state_across_relaunch()`) SKIPS the uninstall: `simctl
        # install` over an existing install upgrades in place and keeps the data
        # container, so Application Support — the nest-identity pin store, the
        # MLS db, SwiftData — survives into the relaunched process, which is the
        # at-rest half of the preserve contract. The terminate still runs: this
        # is a force-quit + relaunch, not a warm restart.
        _run_simctl(["xcrun", "simctl", "terminate", udid, self.BUNDLE_ID],
                    timeout=_SIMCTL_CONTROL_S, what="terminate", best_effort=True)
        # The replica half of the principal-slot carry (convention 10,
        # `HttpBridgeDriver._harvest_replica`) must read the previous launch's
        # container BEFORE the uninstall below destroys it — the slot itself
        # rides a host-side `keychain.json` and is harvested with everything
        # else in `_resolve_credential_store` further down, where the container
        # is already gone. Harvesting the slot twice is idempotent (the newest
        # launch holding an actor wins, and it is the same launch).
        self._harvest_principal_slots(
            getattr(self, "_resolved_credential_dir", None),
            getattr(self, "_resolved_keyring_app", None),
        )
        if not self._preserve_container:
            _run_simctl(["xcrun", "simctl", "uninstall", udid, self.BUNDLE_ID],
                        timeout=_SIMCTL_CONTROL_S, what="uninstall", best_effort=True)
        install = _run_simctl(["xcrun", "simctl", "install", udid, app_path],
                              timeout=_SIMCTL_INSTALL_S, what="install")
        if install.returncode != 0:
            raise RuntimeError(
                f"simctl install failed (rc={install.returncode}): "
                f"{install.stderr.strip()}"
            )
        # Resolve the container here, and seed the pre-launch at-rest fixture (if
        # any) into it while the app is still stopped. Without a preserve pin the
        # container exists only after this install and is thrown away by the next
        # one; with one, this resolves the SAME container the previous launch used.
        # The TCC grant goes HERE — between install and launch. `simctl privacy`
        # names the app by bundle id, so the app must already be installed; and
        # changing a running app's TCC entry restarts it, so it must not be
        # running yet. Best-effort by design: an older Xcode without
        # `simctl privacy` should surface as the test's own "the controls never
        # appeared" failure, not as an opaque driver crash during launch.
        if self._grant_photos:
            # And it must PROVE it worked, the same way `add_photo_to_library`
            # proves `addmedia` did. This was `best_effort=True` until 2026-09-02,
            # and that silence cost a whole diagnosis: the app came up reporting
            # PhotoKit `notDetermined`, `PHAsset.fetchAssets` returned nothing, and
            # from outside the app that is indistinguishable from a real ingest bug
            # — which is exactly what it was mistaken for. A fixture step that can
            # fail invisibly is not a fixture step, it is a coin flip
            # (`test_photo_backup_library_ingest.py`).
            grant = _run_simctl(
                ["xcrun", "simctl", "privacy", udid, "grant", "photos",
                 self.BUNDLE_ID],
                timeout=_SIMCTL_CONTROL_S,
                what="privacy grant photos",
            )
            if grant.returncode != 0:
                raise RuntimeError(
                    "simctl privacy grant photos failed "
                    f"(rc={grant.returncode}) for {self.BUNDLE_ID}: "
                    f"{(grant.stderr or '').strip() or '<no stderr>'}\n"
                    "Without the grant PhotoKit stays notDetermined, its "
                    "authorization prompt is a SpringBoard alert no in-process "
                    "driver can answer, and every photo-backup pass silently "
                    "sees an empty library."
                )
            self._finish_photos_grant(udid)
        self._resolve_app_support(udid)
        self._mark_log_baseline()
        self._seed_app_support(config)

        # The credential store the app's `KeychainStore.e2eFileURL` reads — resolved
        # (and pre-seeded) in the store half both apple drivers share, and forwarded
        # below like the agent port.
        tmp = tempfile.mkdtemp(prefix="fauna-e2e-ios-agent-")
        self._tmp_dir = tmp
        cred_dir = self._resolve_credential_store(config, tmp)
        # The unified account-store root THIS launch resolved: under e2e the app
        # hands shared Rust its container's Application Support as the store
        # container (`AccountStateDir.storeContainerDir` → `SyncStateDir.
        # appSupportSyncDir`, `<Application Support>/Fauna/sync`). Published after the
        # carry began in `_resolve_credential_store`, so the harvest read the
        # launch being replaced; `None` when the container could not be located,
        # which reads as "no replica to carry", never as a path.
        self._resolved_store_root = (
            os.path.join(self._app_support, "Fauna", "sync") if self._app_support else None
        )

        # Pass the per-instance env through simctl's SIMCTL_CHILD_* forwarding:
        # `SIMCTL_CHILD_FOO=bar` in simctl's environment reaches the launched app
        # as `FOO=bar`. The sim shares the host loopback, so the app binds
        # 127.0.0.1:port and the host reaches it at the same address.
        env = dict(os.environ)
        env["SIMCTL_CHILD_FAUNA_E2E_AGENT_PORT"] = str(port)
        env["SIMCTL_CHILD_FAUNA_E2E_CREDENTIAL_DIR"] = cred_dir
        # Where FaunaApp saves e2e-mode downloads (SnapshotFileSaver.e2eDownloadDir)
        # — a caller override via config["environment"] wins (forwarded below by
        # the generic loop too; setting it explicitly here lets us also record it
        # in `self._download_dir` for `download_dir()`), else a fresh per-launch
        # tmp subdir. Mirrors `_cred_dir` above and `MacosInProcessDriver.launch`.
        download_dir = (config.get("environment") or {}).get(
            "FAUNA_E2E_DOWNLOAD_DIR"
        ) or os.path.join(tmp, "downloads")
        os.makedirs(download_dir, exist_ok=True)
        self._download_dir = download_dir
        env["SIMCTL_CHILD_FAUNA_E2E_DOWNLOAD_DIR"] = download_dir
        # The home-screen widget snapshot dir — the twin of the macOS launch
        # (`MacosInProcessDriver.launch`).
        widget_dir = (config.get("environment") or {}).get(
            "FAUNA_E2E_WIDGET_DIR"
        ) or os.path.join(tmp, "widget")
        os.makedirs(widget_dir, exist_ok=True)
        self._widget_dir = widget_dir
        env["SIMCTL_CHILD_FAUNA_E2E_WIDGET_DIR"] = widget_dir
        if config.get("url"):
            env["SIMCTL_CHILD_FAUNA_E2E_NEST_URL"] = config["url"]
        # The resident engine's full-reconcile cadence (`always_resident::rescan_interval`,
        # the compile-gated `FAUNA_E2E_RESCAN_MS` seam) — linux's, tui's and (since
        # 2026-08-25) macOS's twin of this launch all default it. iOS's `apple-ffi-test`
        # build (release profile, so `debug_assertions` is false) only started reading
        # the var once `fauna-ffi/test-helpers` forwarded `fauna-sync-engine/e2e-agent`
        # (2026-08-27) — until then this override was inert on
        # iOS, unlike its siblings. `setdefault` before the override loop below so a
        # caller-supplied `environment["FAUNA_E2E_RESCAN_MS"]` still wins.
        env.setdefault("SIMCTL_CHILD_FAUNA_E2E_RESCAN_MS", "30000")
        for k, v in (config.get("environment") or {}).items():
            env[f"SIMCTL_CHILD_{k}"] = str(v)

        # `config["app_args"]` — the iOS twin of `MacosInProcessDriver`'s own
        # `shlex.split(config.get("app_args", ""))` argv passthrough (e.g.
        # `-AppleLocale en_GB`, the standard Foundation argument-domain seam a
        # test uses to override `Locale.current`/`Calendar.current` without any
        # product-side env knob). `simctl launch <device> <bundle-id> [argv...]`
        # forwards trailing tokens to the launched process the same way.
        launch_argv = shlex.split(config.get("app_args", "") or "")

        launch = _run_simctl(
            ["xcrun", "simctl", "launch", udid, self.BUNDLE_ID, *launch_argv],
            timeout=_SIMCTL_BOOT_S, what="launch", env=env,
        )
        if launch.returncode != 0:
            raise RuntimeError(
                f"simctl launch failed (rc={launch.returncode}): "
                f"{launch.stderr.strip()}"
            )

        # `simctl launch` (without --console) prints "<bundle-id>: <pid>" — a
        # REAL host PID (empirically verified: `ps -p <pid>` resolves the
        # Simulator app binary under CoreSimulator/.../Bundle/Application/...,
        # and SIGKILLing it terminates the process cleanly). This is what makes
        # `kill_uncleanly()` possible for a driver that doesn't hold the Popen.
        self._app_pid = None
        m = re.search(r":\s*(\d+)\s*$", launch.stdout.strip())
        if m:
            self._app_pid = int(m.group(1))

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self._agent_health_ok():
                self._bridge_dead = False
                return
            time.sleep(0.3)
        raise RuntimeError(
            f"iOS in-process agent never became healthy on {self._url}.\n"
            f"For diagnostics, launch with the console attached:\n"
            f"  xcrun simctl launch --console {udid} {self.BUNDLE_ID}"
        )

    def teardown(self) -> None:
        """Terminate the app in the Simulator (best-effort). A no-op `terminate`
        on an already-`kill_uncleanly()`-killed PID is harmless (best-effort,
        `check=False`) — mirrors the Popen drivers tolerating a dead child."""
        config = getattr(self, "_launch_config", None)
        udid = (config or {}).get("udid")
        if udid:
            _run_simctl(
                ["xcrun", "simctl", "terminate", udid, self.BUNDLE_ID],
                timeout=_SIMCTL_CONTROL_S, what="terminate", best_effort=True,
            )
        self._app_pid = None

    def dismiss_system_dialog(self) -> None:
        """No system dialogs under the in-process driver (no XCUITest)."""
        pass
