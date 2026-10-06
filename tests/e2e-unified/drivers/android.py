"""Android driver using on-device Kotlin bridge via ADB port-forward."""

import json
import os
import shutil
import subprocess
import time
from pathlib import Path
from urllib.parse import urlparse

from .http_bridge import HttpBridgeDriver
from .port_util import drain_pipes, popen_group_kwargs, reap_descendants_of


def nest_ports_from_config(config: dict) -> list[int]:
    """Every nest port this launch config names, in first-seen order.

    Two sources, because a run can have more than one nest and they arrive by
    different routes (`testing.md` § Default app and nest mode → *N nests per
    run*): the session nest rides in as `url`, and any additional nest the
    harness knows about at launch time rides in as `nest_ports`. Both are read
    so nothing here hardcodes "the one nest" -- a second nest that never got a
    reverse is a journey that dies at its first cross-nest call with a connection
    error naming nothing.

    A nest started AFTER the app launched cannot appear here by construction;
    `AndroidBridgeDriver.ensure_reverse` is how the harness adds it.
    """
    ports: list[int] = []

    def add(value):
        try:
            port = int(value)
        except (TypeError, ValueError):
            return
        if port > 0 and port not in ports:
            ports.append(port)

    url = config.get("url")
    if url:
        add(urlparse(url).port)
    for extra in config.get("nest_ports") or ():
        add(extra)
    return ports


class AndroidBridgeDriver(HttpBridgeDriver):
    """Connects to the Kotlin bridge on an Android device via ADB port-forward."""

    def log_scope_across_relaunch(self) -> str:
        """``"none"`` — this driver answers neither `app_log_text` nor `app_stderr_text` .

        See the base declaration for what each answer means and why it is
        declared rather than inferred; pinned per driver by
        `tests/test_module_relaunch.py`.
        """
        return "none"

    def __init__(self):
        super().__init__()
        self._bridge_proc = None
        self._local_port = None
        self._device_port = None
        self._device_serial = None
        # The launch config's `adb_server`: a socket spec naming a remote adb
        # server this driver is a pure client of, or None for a local one.
        self._adb_server = None
        self._forward_lease = None
        # Every port this driver has an `adb reverse` open for. Its OWN list,
        # deliberately not derived from `_local_port` at teardown: that port is
        # the FORWARD's (host -> device, for the bridge), and the reverses run
        # the other way for the nests. Keying removal on the forward would leave
        # every reverse claimed on the device against the next run -- the same
        # class of leak the `am instrument -w` reap note above guards.
        self._reverse_ports: list[int] = []
        # The host directory `download_dir()` mirrors the device's into; made on
        # first use, removed at teardown.
        self._download_mirror: str | None = None
        # name -> (size, mtime_ms) of each file as last fetched into the mirror.
        self._download_mirrored: dict[str, tuple[int, int]] = {}

    # The Kotlin bridge serves POST /element/scroll-into-view
    # (`ElementOps.scrollIntoView`: the accessibility ACTION_SHOW_ON_SCREEN,
    # Compose's bringIntoView), so wait_for and the day-timeline measurement
    # can bring an offscreen element into view directly.
    _supports_scroll_into_view = True

    def is_mobile(self) -> bool:
        return True

    def is_android(self) -> bool:
        return True

    def get_attr(
        self, element_id: str, attribute: str, index: int = 0, *, scope: str | None = None
    ) -> str | None:
        """The bridge's attribute read, but for the compose field's ``text-runs``.

        The compose field's applied styling is the AnnotatedString its
        VisualTransformation hands the field, which the accessibility tree does not
        carry. So ``text-runs`` on ``dm-text-field`` is the app's ``compose_text_runs``
        command, which serializes what the field last applied in linux's JSON shape
        (``appliedRuns`` in ``MarkdownCompose.kt``) — windows's route for the same
        attribute. Returned as the JSON string the other drivers return."""
        if attribute == "text-runs" and element_id == "dm-text-field":
            runs = self.call_command("compose_text_runs")
            return None if runs is None else json.dumps(runs)
        return super().get_attr(element_id, attribute, index, scope=scope)

    #: Ids whose ``type_text`` the UiAutomator bridge cannot perform, because the
    #: typed text lands in a hosted native widget with no test id rather than in
    #: a field. ``dm-reaction-more-button``: the fuller reaction picker is the
    #: emoji2 ``EmojiPickerView`` (``ui/conversations.md`` § Reactions & message
    #: delete → *Rendering / picker glue*).
    AGENT_TYPED_TARGETS = frozenset({"dm-reaction-more-button"})

    def type_text(self, element_id: str, text: str, *, scope: str | None = None) -> None:
        """The bridge's typing, but for a hosted native chooser.

        The typed text for an ``AGENT_TYPED_TARGETS`` id goes to the app's agent as
        a ``type_text`` patch, which performs the chooser's own pick
        (``TestAgent.kt::pickMoreReaction``) — the way ``set_input_files`` routes a
        file pick the bridge cannot drive through ``compose.file``. The agent
        refuses a pick with no picker open, so the click that opens it stays part
        of the journey."""
        if element_id in self.AGENT_TYPED_TARGETS:
            self.set_state({"type_text": {"target": element_id, "text": text}})
            return
        super().type_text(element_id, text, scope=scope)

    #: The bridge route that writes a picked file onto the device
    #: (``BridgeHttpServer.kt`` → ``AppLauncher.writeInputFile``).
    INPUT_FILE_ROUTE = "/input-file"

    def set_input_files(self, element_id: str, files: str | list[str]) -> None:
        """The shared compose-state staging, but with a path that exists ON THE DEVICE.

        Every other bridge-backed app runs on the pytest host, so the shared
        ``set_input_files`` hands its agent the host path. android's agent
        (``TestAgent.kt``'s ``compose.file`` arms) opens ``java.io.File(path)`` on
        the device, where no host path exists on any venue -- under venue A2 the
        device is not even on this machine (`testing.md` § Default app and nest
        mode → *Android's run venue*). So the file's bytes ride the bridge's own
        HTTP connection first, the same boundary crossing ``seed_credentials``
        makes: the bridge (same uid as the app) writes them under the app's cache
        dir, keeping the basename -- the agent derives the attachment's name and
        MIME from it -- and answers with that device path, which is all the
        ``compose`` patch ever carries. No ``adb push``, so nothing depends on
        what an app may read outside its own sandbox.
        """
        path = files if isinstance(files, str) else files[0]
        self.set_state({"compose": {"file": self.push_input_file(path), "target": element_id}})

    def push_input_file(self, host_path: str) -> str:
        """Copy ``host_path``'s bytes onto the device; return the device path.

        Raw bytes, not base64 in a JSON body: an oversized-attachment leg sends
        a 15 MiB file, which base64 would inflate by a third through the
        on-device server. Raises ``FileNotFoundError`` before touching the bridge
        for a missing host file, and ``RuntimeError`` when the bridge answers
        without a device path -- never falls back to the host path, which would
        only move the failure to the agent's "could not read"."""
        from urllib.parse import quote

        host = Path(host_path)
        data = host.read_bytes()
        reply = self._post_raw(
            f"{self.INPUT_FILE_ROUTE}?name={quote(host.name, safe='')}",
            data,
            "application/octet-stream",
        )
        device_path = reply.get("path") if isinstance(reply, dict) else None
        if not isinstance(device_path, str) or not device_path:
            raise RuntimeError(
                f"bridge POST {self.INPUT_FILE_ROUTE} answered without a device path: {reply!r}"
            )
        return device_path

    def launch(self, config: dict) -> None:
        from helpers import android_venue

        from .port_util import find_free_port

        app_apk = config.get("app_path")
        test_apk = config.get("test_apk")
        self._device_serial = config.get("device_serial")
        self._adb_server = config.get("adb_server")
        self._device_port = config.get("bridge_port", 18500)

        adb = self._adb_cmd()

        # Install APKs
        if app_apk:
            subprocess.run(
                [*adb, "install", "-r", "-t", app_apk],
                check=True, capture_output=True, timeout=60,
            )
        if test_apk:
            subprocess.run(
                [*adb, "install", "-r", "-t", test_apk],
                check=True, capture_output=True, timeout=60,
            )

        # ADB port-forward: local port -> device port. `adb forward` listens on
        # the machine running the adb SERVER. With a local server that is this
        # machine and any free port will do; with a remote one (`adb_server`,
        # the tunnelled venue) it is the other end of the tunnel, so the port
        # must be one the tunnel already carries -- a leased port of the venue's
        # fixed range (`helpers/android_venue.py`), never `find_free_port()`.
        if self._adb_server:
            self._forward_lease = android_venue.lease_bridge_forward_port()
            self._local_port = self._forward_lease.port
        else:
            self._local_port = find_free_port()
        subprocess.run(
            [*adb, "forward", f"tcp:{self._local_port}", f"tcp:{self._device_port}"],
            check=True, capture_output=True,
        )

        # ADB REVERSE, the other direction: the DEVICE's own 127.0.0.1:<p> is
        # carried back to <p> on the machine running the adb server, where the
        # test nest listens. This is constraint 3 of testing.md § Default app and
        # nest mode → *Android's run venue*, and it is what lets the app be
        # handed the harness's ordinary `http://127.0.0.1:<port>` URL with no
        # rewriting anywhere -- the device's loopback IS the nest.
        #
        # ⚠ The reverse lands on the adb SERVER's loopback, not on the machine
        # that happens to be driving pytest. They are the same box for a local
        # emulator or a USB device; under the remote-adb venue they are NOT, and
        # the tunnel carries the venue's fixed nest-port range the rest of the
        # way -- which is why `ensure_reverse` refuses any other port there.
        #
        # Before the app is launched, deliberately: the /session POST below
        # starts it, and an app that reaches its first nest call before the
        # reverse exists fails with a bare connection error that names nothing.
        for port in nest_ports_from_config(config):
            self.ensure_reverse(port)

        # Start bridge instrumentation on device (runs in background)
        # Armed against point 9 (2026-08-14): `am instrument -w` streams for the
        # WHOLE run, so this adb client is long-lived by construction, and an
        # unreaped one keeps its device port-forward claimed against the next run.
        self._bridge_proc = subprocess.Popen(
            [*adb, "shell", "am", "instrument", "-w",
             "-e", "port", str(self._device_port),
             # Test-APK package = applicationId + ".test"; the runner CLASS
             # keeps the source-package name (namespace stays com.fauna.app).
             "social.fauna.fauna.test/com.fauna.app.bridge.BridgeInstrumentation"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            **popen_group_kwargs(),
        )
        reap_descendants_of(self._bridge_proc.pid)
        # `am instrument -w` streams for the whole run and NOTHING here ever read
        # it, so this pipe was guaranteed to fill and then block the adb client
        # (port_util.drain_pipes — same class as the windows bridge stall).
        self._bridge_log = drain_pipes(self._bridge_proc)

        self._url = f"http://127.0.0.1:{self._local_port}"
        self._wait_for_health(timeout=30)

        # Kept for `preserve_state_across_relaunch()`, which pins into it, and
        # for the relaunch a real `recover()` will run from it.
        self._launch_config = config
        self._post("/session", self._session_body(config))

    #: The `_launch_config` key `preserve_state_across_relaunch()` pins. android
    #: has no per-launch store dir to pin (the base vocabulary,
    #: `_RELAUNCH_PIN_KEYS`): the app's filesDir already survives every
    #: relaunch, and the one thing that would replace the store is the
    #: launch's own `seed_credentials` overwrite — so the pin is "don't seed".
    _PRESERVE_STORE_KEY = "preserve_store"

    def _session_body(self, config: dict) -> dict:
        """The `/session` POST body that launches the app for `config`."""
        env = config.get("environment", {})
        env["FAUNA_E2E_BRIDGE"] = f"http://127.0.0.1:{self._device_port}"
        body = {
            "app": "social.fauna.fauna",
            "environment": env,
        }
        if config.get(self._PRESERVE_STORE_KEY):
            # Pinned: the relaunched process must read back what the previous
            # one wrote, so the seed — a full overwrite of the credential file
            # (`AppLauncher.launchApp`) — is withheld. The bridge still
            # forwards the file the app left behind.
            return body
        # Multi-account registry seed (tests/common/accounts.py::build_registry_seed).
        # Unlike windows/linux, no host-side file
        # is written here: the seed rides the same HTTP boundary as `environment`
        # and BridgeHttpServer/AppLauncher (same process/UID as the app under
        # test) writes it to the app's own filesDir before launch -- see
        # AppLauncher.launchApp's credentialFile() doc.
        #
        # `is not None`, not truthiness: an EMPTY seed is a real instruction --
        # "this launch has no stored identity" -- that the bridge writes over
        # whatever file an earlier launch left in filesDir (which the bridge
        # would otherwise keep forwarding). No key at all leaves the file alone.
        seed = config.get("seed_credentials")
        if seed is not None:
            body["seed_credentials"] = dict(seed)
        return body

    def preserve_state_across_relaunch(self) -> bool:
        """Pin this launch's client-local store across the next relaunch — the
        android twin of `IOSDriver.preserve_state_across_relaunch`; see the base
        contract.

        **False while this driver cannot relaunch at all.** android's `recover()`
        is still the base bridge health-probe (`supports_cold_relaunch()` is
        False), so "the store
        survives the relaunch" has no relaunch to survive, and a True here would
        turn every caller's honest skip into a test that asserts a relaunch that
        never happened. The pin itself is ready: once `recover()` relaunches
        from `_launch_config`, the pinned `_PRESERVE_STORE_KEY` withholds the
        seed overwrite and this answers True with no further change.
        """
        config = getattr(self, "_launch_config", None)
        if config is None or not self.supports_cold_relaunch():
            return False
        config[self._PRESERVE_STORE_KEY] = True
        # So reset()'s `_clear_relaunch_pin` un-pins it, and `hard_reload` takes
        # its pinned contract (wait for the app's own auto-login).
        self._record_relaunch_pin(config, (self._PRESERVE_STORE_KEY,))
        return True

    def teardown(self) -> None:
        try:
            self._delete("/session")
        except Exception:
            pass
        if self._bridge_proc:
            self._bridge_proc.terminate()
            try:
                self._bridge_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._bridge_proc.kill()
                self._bridge_proc.wait()
            self._bridge_proc = None
        if self._local_port:
            subprocess.run(
                [*self._adb_cmd(), "forward", "--remove", f"tcp:{self._local_port}"],
                check=False, capture_output=True,
            )
            self._local_port = None
        if self._forward_lease:
            self._forward_lease.release()
            self._forward_lease = None
        # Each reverse is removed by the port it was opened for, from this
        # driver's own record -- see `_reverse_ports`. `check=False` for the same
        # reason the forward's removal uses it: teardown must not raise over a
        # mapping the device already dropped (a reboot, a detach, a crashed adb
        # server), or it masks whatever the test was actually reporting.
        for port in self._reverse_ports:
            subprocess.run(
                [*self._adb_cmd(), "reverse", "--remove", f"tcp:{port}"],
                check=False, capture_output=True,
            )
        self._reverse_ports = []
        if self._download_mirror:
            shutil.rmtree(self._download_mirror, ignore_errors=True)
            self._download_mirror = None
            self._download_mirrored = {}

    def credential_map(self) -> dict[str, str]:
        """The on-device e2e credential file, read back over the bridge.

        The file (`AppLauncher`'s ``credentialFile()``, which the app's
        ``FileSecretBackend`` reads and writes) lives in the app's own filesDir,
        which no host path reaches -- so ``GET /credentials`` is the read half of
        the same boundary crossing ``seed_credentials`` is the write half of. The
        flat ``{native_key: value}`` map, keyed exactly as the app stores it
        (every logical key -- ``fauna/index``, the per-actor slots --
        verbatim); ``{}`` when no file exists.

        A reply without the ``credentials`` object raises rather than reading
        as empty: every caller treats an empty map as a real answer.
        """
        reply = self._get("/credentials")
        creds = reply.get("credentials") if isinstance(reply, dict) else None
        if not isinstance(creds, dict):
            raise RuntimeError(f"bridge GET /credentials answered without a credentials map: {reply!r}")
        return creds

    def download_dir(self) -> str | None:
        """A host directory mirroring the app's on-device download directory, as of this call.

        Every other app saves e2e downloads into a directory on the pytest host,
        which its driver simply names. android's app writes into its own cache
        (``cacheDir/fauna`` — the one directory every android download surface
        saves into before offering the file to the share sheet,
        ``ui/util/ShareFile.kt``), which no host path reaches on any venue. So
        the files cross the way ``credential_map`` reads the credential file:
        over the bridge's own HTTP connection (``GET /download-dir`` lists,
        ``GET /download-file`` hands one file's bytes back), into a per-driver
        host directory that is made to match the device's — new and changed
        files fetched, files the device no longer has removed, so "nothing was
        left behind" is as observable as "a file appeared".

        ⚠ The mirror is refreshed by THIS call and by nothing else: a wait
        that polls for a download must call ``download_dir()`` inside its
        predicate, not once before it. On the host-filesystem drivers the
        repeated call is free. None before ``launch()`` gave the driver a
        bridge.
        """
        if not self._url:
            return None
        if self._download_mirror is None:
            import tempfile

            self._download_mirror = tempfile.mkdtemp(prefix="fauna-e2e-android-downloads-")
        mirror = self._download_mirror
        reply = self._get("/download-dir")
        files = reply.get("files") if isinstance(reply, dict) else None
        if not isinstance(files, list):
            raise RuntimeError(f"bridge GET /download-dir answered without a files list: {reply!r}")
        on_device: dict[str, tuple[int, int]] = {}
        for entry in files:
            name = entry.get("name") if isinstance(entry, dict) else None
            # A device file name is a bare name by construction (one directory,
            # listed flat); anything else would write outside the mirror.
            if not isinstance(name, str) or not name or name in (".", "..") or "/" in name or "\\" in name:
                raise RuntimeError(f"bridge GET /download-dir listed a non-bare file name: {entry!r}")
            on_device[name] = (int(entry["size"]), int(entry["mtime_ms"]))
        for stale in set(os.listdir(mirror)) - set(on_device):
            os.remove(os.path.join(mirror, stale))
            self._download_mirrored.pop(stale, None)
        for name, stamp in on_device.items():
            local = os.path.join(mirror, name)
            # Size AND modification time: a same-day re-download overwrites the
            # archive under the same name, possibly at the same length.
            if self._download_mirrored.get(name) == stamp and os.path.isfile(local):
                continue
            try:
                data = self._get_bytes("/download-file", {"name": name})
            except LookupError:
                # Gone between the listing and the fetch (a `.part` renamed into
                # place, a refused archive deleted): the next call sees the
                # settled directory.
                if os.path.exists(local):
                    os.remove(local)
                self._download_mirrored.pop(name, None)
                continue
            with open(local, "wb") as fh:
                fh.write(data)
            self._download_mirrored[name] = stamp
        return mirror

    def press_system_back(self) -> None:
        """Send the system back — the phone's back gesture/button — through the
        bridge's ``POST /device/back`` (``ElementOps.pressBack``, UiAutomator's
        ``pressBack``). The current screen's own ``BackHandler`` handles it, so a
        journey stepping out of a pushed screen goes through the same path a user
        does; the android twin of the iOS test agent's ``nav_back`` pop."""
        self._post("/device/back", {})

    def write_reauth_verdict(self, verdict: str | None) -> None:
        """Write (or, for ``None``, remove) the re-auth e2e seam's verdict file.

        ``AccountReauth.confirmActivation`` reads ``reauth-result`` beside the
        e2e credential file, per prompt, instead of showing the real
        BiometricPrompt: ``"approve"`` approves, anything else -- an absent file
        included -- declines, fail-closed. The android twin of the apple/windows
        drivers' host-side ``{cred_dir}/reauth-result`` write; the file is
        on-device, so the bridge writes it (``POST /reauth-result``).
        """
        self._post("/reauth-result", {"verdict": verdict})

    def ensure_reverse(self, port: int) -> None:
        """Open `adb reverse tcp:<port> tcp:<port>`, once, and record it.

        Public because a nest can be started AFTER the app launched -- the
        multi-nest fixtures do exactly that -- and such a nest is invisible to
        `launch()`'s config. Idempotent: a port already reversed by this driver
        is a no-op, so a fixture may call it without knowing what launch already
        did.
        """
        port = int(port)
        if port in self._reverse_ports:
            return
        if self._adb_server:
            from helpers import android_venue

            android_venue.require_tunnelled_nest_port(port)
        subprocess.run(
            [*self._adb_cmd(), "reverse", f"tcp:{port}", f"tcp:{port}"],
            check=True, capture_output=True,
        )
        self._reverse_ports.append(port)

    def _adb_cmd(self) -> list[str]:
        # The one home of the prefix, shared with the availability probe: the
        # executable (PATHEXT-aware), this launch's adb server and its device.
        from helpers import android_device

        return android_device.adb_argv(self._device_serial, self._adb_server)

    def _wait_for_health(self, timeout: float = 30.0):
        import urllib.request
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._bridge_proc and self._bridge_proc.poll() is not None:
                # Read the DRAINED buffer, not the pipe: drain_pipes owns the
                # streams from launch onward, so a direct .read() here would race
                # it and report nothing.
                tail = "\n".join(getattr(self, "_bridge_log", []) or [])
                raise RuntimeError(
                    f"Bridge instrumentation exited early.\nOUTPUT: {tail[-2000:]}"
                )
            try:
                resp = urllib.request.urlopen(f"{self._url}/health", timeout=2)
                if resp.status == 200:
                    return
            except Exception:
                pass
            time.sleep(0.5)
        raise TimeoutError(f"Android bridge not ready after {timeout}s at {self._url}")
