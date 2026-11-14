"""Android driver using on-device Kotlin bridge via ADB port-forward."""

import contextlib
import io
import json
import os
import re
import shutil
import subprocess
import tarfile
import time
from pathlib import Path
from urllib.parse import urlparse

from .http_bridge import HttpBridgeDriver, LaunchWitnesses, launch_timeout_report
from .local_launch import schedstat_wait_s, stat_cpu
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


def _device_cpu_reading(text: str) -> tuple[str, float | None, float | None, str]:
    """(pid, CPU seconds, wall seconds since the process started, why unread) from
    the device's answer to `_launch_witnesses`' shell: `pid <n>`, `CLK_TCK`, the
    app's `/proc/<pid>/stat` line and `/proc/uptime`."""
    lines = [ln.strip() for ln in text.splitlines() if ln.strip()]
    pid = next((ln.split()[1] for ln in lines if ln.startswith("pid ") and len(ln.split()) > 1), "")
    if not pid:
        return "", None, None, "the app has no process on the device (pidof found none)"
    stat = next((ln for ln in lines if "(" in ln and ")" in ln), None)
    hz = next((int(ln) for ln in lines if ln.isdigit()), None)
    uptime = next((ln for ln in lines if re.fullmatch(r"[\d.]+ [\d.]+", ln)), None)
    if stat is None or hz is None or uptime is None:
        return pid, None, None, f"the device's /proc answer was incomplete: {text[-200:]!r}"
    try:
        cpu_s, since_boot = stat_cpu(stat, hz)
    except (ValueError, IndexError) as exc:
        return pid, None, None, f"the device's /proc/{pid}/stat could not be parsed ({exc})"
    return pid, cpu_s, float(uptime.split()[0]) - since_boot, ""


def _device_wait_reading(text: str, pid: str, wall_s: float | None) -> tuple[float | None, str, str]:
    """(share of the window waited for a CPU, how it was read, why unread) from the
    `schedstat` lines in the same answer: the device's `/proc/<pid>/task/*/schedstat`,
    the kernel's own run-queue wait, read as linux's is."""
    if not wall_s:
        return None, "", "no wall time to take the wait's share of"
    wait_s = schedstat_wait_s(ln.strip() for ln in text.splitlines())
    if wait_s is None:
        return None, "", f"the device answered no schedstat line for /proc/{pid}/task/* (CONFIG_SCHED_INFO off?)"
    return (
        wait_s / wall_s,
        f"its longest-waiting thread's run-queue wait, {wait_s:.1f} s of {wall_s:.1f} s, "
        f"from the device's /proc/{pid}/task/*/schedstat",
        "",
    )


# --- Convention 6's close rider: the device's crash witnesses ----------------

#: A `logcat -v epoch` line: `<epoch>.<frac>  <pid>  <tid> <level> <tag>: <message>`.
_LOGCAT_EPOCH_LINE = re.compile(
    r"^\s*(?P<at>\d+\.\d+)\s+(?P<pid>\d+)\s+(?P<tid>\d+)\s+(?P<level>[VDIWEFA])\s+"
    r"(?P<tag>[^:]*?)\s*:\s?(?P<msg>.*)$"
)

#: The three lines a crash block opens with: the runtime's uncaught exception
#: (`AndroidRuntime`), libc's report of a fatal signal (a native crash, a Rust
#: `abort()`), and debuggerd's tombstone header.
_CRASH_OPENERS = re.compile(r"FATAL EXCEPTION|Fatal signal \d+|^\*\*\* \*\*\* \*\*\*")

#: How many lines of one block a report carries: a Java trace with its causes
#: runs to a few dozen, a tombstone's header and backtrace to about a hundred.
_CRASH_BLOCK_MAX_LINES = 160


def _device_instant_reading(text: str) -> tuple[float | None, str]:
    """(the device's epoch clock, the app's pid or ``""``) from the answer to
    `date +%s.%N; echo "pid $(pidof <package>)"` — the one adb call every
    launch and every close makes. An unread clock is ``None``; toybox `date`
    printing `%N` literally still yields the whole seconds."""
    at = None
    pid = ""
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("pid"):
            pid = line[3:].strip().split(" ")[0] if line[3:].strip() else ""
        elif at is None:
            m = re.match(r"^(\d+)(?:\.(\d+))?", line)
            if m:
                at = float(f"{m[1]}.{m[2]}") if m[2] else float(m[1])
    return at, pid


def _crash_block(
    logcat: str, *, package: str, pid: str, since: float | None
) -> tuple[str, list[str], float] | None:
    """The first crash block in ``logcat`` (`logcat -d -b main,crash -v epoch`)
    that belongs to this app and was written at or after ``since``, as
    (its fatal line, the block whole, its device instant) — or ``None``.

    A block belongs to the app when its opening line was written by the app's
    process (``pid``, the pid read at its launch) or names the package — the
    runtime's `Process: <package>, PID: <n>`, libc's `pid <n> (<package>)`,
    the tombstone's `>>> <package> <<<`. The pid is what keeps another app's
    crash, or this app's crash in an earlier launch, out; the package name is
    the fallback for a launch whose pid was unread. The harness's own
    `am force-stop` writes no block, so it never reads as one."""
    parsed = [(_LOGCAT_EPOCH_LINE.match(ln), ln) for ln in logcat.splitlines()]
    for i, (m, raw) in enumerate(parsed):
        if m is None or not _CRASH_OPENERS.search(m["msg"]):
            continue
        at = float(m["at"])
        if since is not None and at < since:
            continue
        block = [raw]
        for nm, nraw in parsed[i + 1:]:
            if len(block) >= _CRASH_BLOCK_MAX_LINES:
                break
            if nm is None or (nm["pid"], nm["tag"]) != (m["pid"], m["tag"]):
                continue
            if _CRASH_OPENERS.search(nm["msg"]) and nm["msg"] != m["msg"]:
                break
            block.append(nraw)
        text = "\n".join(block)
        owned = (pid and m["pid"] == pid) or any(
            marker in text
            for marker in (f"Process: {package},", f"({package})", f">>> {package} <<<")
        )
        if not owned:
            continue
        return _fatal_line(m["msg"], block), block, at
    return None


def _fatal_line(opener: str, block: list[str]) -> str:
    """The one line a death report leads with: the exception the runtime
    reported (`FATAL EXCEPTION: main — java.lang.IllegalStateException: …`),
    libc's fatal-signal line, or a tombstone's `signal N (SIG…)` line."""
    messages = [m["msg"] for m in map(_LOGCAT_EPOCH_LINE.match, block[1:]) if m]
    if "FATAL EXCEPTION" in opener:
        cause = next(
            (msg.strip() for msg in messages if msg.strip() and not msg.startswith(("Process:", "PID:"))),
            "",
        )
        return f"{opener.strip()} — {cause}" if cause else opener.strip()
    if opener.startswith("***"):
        signal_line = next((msg.strip() for msg in messages if re.match(r"\s*signal \d+", msg)), "")
        return f"tombstone — {signal_line}" if signal_line else "tombstone (no signal line in its header)"
    return opener.strip()


class AndroidBridgeDriver(HttpBridgeDriver):
    """Connects to the Kotlin bridge on an Android device via ADB port-forward."""

    def log_scope_across_relaunch(self) -> str:
        """``"per-launch"`` — the reader slices from a byte floor taken at launch.

        The app's `fauna_log` rolling files live in its `filesDir`, which survives
        every relaunch and `adb install -r`, so the files are cumulative across
        runs; `_mark_log_baseline()` records each file's size over the bridge
        before `/session` starts the app, and `app_log_text` reads only past it
        (the iOS shape). See the base declaration for what each answer means;
        pinned per driver by `tests/test_module_relaunch.py`.
        """
        return "per-launch"

    def _mark_log_baseline(self) -> None:
        """Record how much log each file already holds, before the app starts.

        Called after the bridge is healthy and before `/session` launches the
        app — every byte counted here belongs to a PREVIOUS launch, with no race
        against the live app.
        """
        self._log_baseline = {}
        for entry in self._app_log_listing():
            self._log_baseline[entry["name"]] = int(entry["size"])

    def _app_log_listing(self) -> list[dict]:
        """The bridge's flat listing of the app's log files (`GET /app-log`)."""
        reply = self._get("/app-log")
        files = reply.get("files") if isinstance(reply, dict) else None
        if not isinstance(files, list):
            raise RuntimeError(f"bridge GET /app-log answered without a files list: {reply!r}")
        return files

    def get_clipboard_text(self) -> str | None:
        """The system clipboard's plain text, read on the device by the
        androidTest bridge's ``GET /clipboard/text`` (``ClipboardManager``, the
        service the app's copy buttons write to) — the windows / linux / apple
        contract: ``None`` when the clipboard holds no text; a bridge refusal
        raises rather than reading empty.

        The consumer is the copy journeys — outcome 13
        (``helpers/recovery_restore.py::copy_the_shown_kit``) — not the
        ceremony outcomes 6-9, which read the secret off the display. The
        clipboard is device-wide, so a caller compares against a read taken
        BEFORE its copy."""
        reply = self._get("/clipboard/text")
        if "error" in reply:
            raise RuntimeError(f"android clipboard read refused: {reply['error']}")
        return reply.get("text")

    # ── The documents provider, as another app's picker drives it ──────────
    # (`DocumentsOps` on the androidTest bridge). The provider is guarded by
    # MANAGE_DOCUMENTS, which `adb shell content` does not hold; the bridge runs
    # in the app's own process, so its ContentResolver calls reach exactly the
    # entry points a picker's do. A provider refusal raises (RuntimeError, or
    # LookupError for a not-found).

    DOCUMENTS_ROOT_ID = "fauna"

    def documents_children(self, parent: str = DOCUMENTS_ROOT_ID) -> list[dict]:
        """``parent``'s children as ``{id, name, flags}`` — the root lists the
        account's presented sets."""
        return list(self._get("/documents/children", {"parent": parent}).get("children", []))

    def documents_create(self, parent: str, name: str) -> str:
        """Create an empty document ``name`` in ``parent`` (``createDocument``);
        its new document id."""
        return self._post("/documents/create", {"parent": parent, "name": name})["document_id"]

    def documents_open_write(self, document_id: str) -> int:
        """Open ``document_id`` for write and hold the descriptor on the device —
        an edit the user has open. ``documents_write_close`` ends it."""
        return int(self._post("/documents/open-write", {"document_id": document_id})["handle"])

    def documents_write_close(self, handle: int, body: bytes) -> None:
        """Replace the held descriptor's bytes with ``body`` and close it — the
        written close the provider ingests."""
        import base64

        self._post(
            "/documents/write-close",
            {"handle": handle, "body_b64": base64.b64encode(body).decode()},
        )

    def documents_write(self, document_id: str, body: bytes) -> None:
        """One open → write → close of ``document_id``."""
        self.documents_write_close(self.documents_open_write(document_id), body)

    def documents_read(self, document_id: str) -> bytes:
        """``document_id``'s bytes as the provider serves them."""
        import base64

        return base64.b64decode(self._get("/documents/read", {"document_id": document_id})["body_b64"])

    def documents_parks(self) -> list[dict]:
        """Each presented set's park read, in-process off the host's state DB:
        ``{name, folder_id, access_revoked, unrecorded}``."""
        return list(self._get("/documents/parks").get("parks", []))

    def app_log_text(self) -> str | None:
        """This launch's on-disk app log — the client's own account of itself,
        attached to every failing test's report by `helpers/app_log_section.py`.

        The android twin of `IosBridgeDriver.app_log_text`: the shared
        `fauna_log` rolling files (`<filesDir>/logs/fauna.log.<date>`,
        `FaunaApp.kt` → `installLogging`), fetched over the bridge because no
        host path reaches the app's `filesDir`. **Not `adb logcat`:** the Rust
        tracing output never reaches logcat (no logcat layer is installed), so a
        logcat reader would return Kotlin `Log.*` noise and miss every
        `mls-sync:` / `account runtime:` line the tests ask about.

        `None` (no files, or bridge unreachable) is a deliberate answer: it makes
        `app_log_section` emit its explicit `<empty>` section.
        """
        baseline = getattr(self, "_log_baseline", None) or {}
        chunks = []
        try:
            listing = self._app_log_listing()
            for entry in listing:
                name = entry["name"]
                data = self._get_bytes(
                    "/app-log-file", {"name": name, "from": baseline.get(name, 0)}
                )
                text = data.decode("utf-8", errors="replace")
                if text:
                    chunks.append(text)
        except Exception:  # noqa: BLE001 — a dead bridge at report time is "no log", not a second failure
            return None
        return "\n".join(chunks) if chunks else None

    def on_device_log_text(self) -> str:
        """The app's own on-device rolling log file(s), whole — every
        `fauna.log.*` under `<filesDir>/logs/` oldest first, or "" — fetched
        over the bridge (`GET /app-log` + `/app-log-file`) because no host path
        reaches the app's `filesDir`. Unlike `app_log_text()` it reads every
        file from its start, not this launch's slice: the journeys that ask
        (`test_settings_logs.py`, `test_app_log_size_cap_tui.py`) read what the
        file holds across launches and prunes. Same contract as linux's and
        tui's host-side readers."""
        chunks = []
        for name in self.on_device_log_sizes():
            data = self._get_bytes("/app-log-file", {"name": name, "from": 0})
            text = data.decode("utf-8", errors="replace")
            if text:
                chunks.append(text)
        return "\n".join(chunks)

    def on_device_log_sizes(self) -> dict[str, int]:
        """Each on-device `fauna.log.*` file's size by name, from the bridge's
        listing of `<filesDir>/logs/` (`GET /app-log`)."""
        return {
            entry["name"]: int(entry.get("size", 0))
            for entry in self._app_log_listing()
            if str(entry.get("name", "")).startswith("fauna.log.")
        }

    def app_stderr_text(self) -> str:
        """This launch's on-disk app log, `""` if it has none yet (the cross-app contract)."""
        return self.app_log_text() or ""

    def __init__(self):
        super().__init__()
        self._bridge_proc = None
        self._local_port = None
        self._device_port = None
        self._device_serial = None
        # The launch config's `adb_server`: a socket spec naming a remote adb
        # server this driver is a pure client of, or None for a local one.
        self._adb_server = None
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
        # The host directory `widget_dir()` mirrors the widget snapshot into;
        # made on first use, removed at teardown.
        self._widget_mirror: str | None = None

    # The Kotlin bridge serves POST /element/scroll-into-view
    # (`ElementOps.scrollIntoView`: the accessibility ACTION_SHOW_ON_SCREEN,
    # Compose's bringIntoView), so wait_for and the day-timeline measurement
    # can bring an offscreen element into view directly.
    _supports_scroll_into_view = True

    def is_mobile(self) -> bool:
        return True

    def is_android(self) -> bool:
        return True

    def set_outbox_drain_hold(self, held: bool) -> None:
        """Set or clear the app process's in-process e2e outbox drain hold
        (`fauna_e2e_agent::OUTBOX_DRAIN_HOLD`). The app hosts its account
        runtime in-process and inherits nothing from the harness, so the hold
        file the desktop drivers set cannot reach it. Driven through
        `helpers/outbox_drain.py`."""
        self.call_command("outbox_drain_hold", {"held": bool(held)})

    #: Document bodies the ``painted`` read answers for (``DocumentBlocks`` under
    #: these test tags) — the ids the ``document-painted-read`` witnesses read.
    PAINTED_DOCUMENT_IDS = frozenset({"feed-post-text", "dm-message-text"})

    def get_attr(
        self, element_id: str, attribute: str, index: int = 0, *, scope: str | None = None
    ) -> str | None:
        """The bridge's attribute read, but for the compose field's ``text-runs`` and a
        document body's ``painted`` lines.

        The compose field's applied styling is the AnnotatedString its
        VisualTransformation hands the field, which the accessibility tree does not
        carry. So ``text-runs`` on ``dm-text-field`` is the app's ``compose_text_runs``
        command, which serializes what the field last applied in linux's JSON shape
        (``appliedRuns`` in ``MarkdownCompose.kt``) — windows's route for the same
        attribute. Returned as the JSON string the other drivers return.

        ``painted`` (``helpers/painted_lines.py``) is what a document body's laid-out
        text drew line by line, which the accessibility tree does not carry either:
        the bridge resolves the element (index and scope included) to its on-screen
        ``frame``, and the app's ``document_painted`` command answers the lines of the
        body painted under that frame (``DocumentPaint.kt``). Returned newline-joined,
        tui's shape; ``None`` when the element is not there."""
        if attribute == "text-runs" and element_id == "dm-text-field":
            runs = self.call_command("compose_text_runs")
            return None if runs is None else json.dumps(runs)
        if attribute == "painted" and element_id in self.PAINTED_DOCUMENT_IDS:
            frame = super().get_attr(element_id, "frame", index, scope=scope)
            if not frame:
                return None
            lines = self.call_command("document_painted", {"frame": frame})
            return None if lines is None else "\n".join(str(line) for line in lines)
        return super().get_attr(element_id, attribute, index, scope=scope)

    def in_viewport(self, element_id: str, *, index: int = 0,
                    scope: str | None = None) -> bool:
        """Whether the ``index``-th ``element_id`` sits in the visible band of the
        open thread: its ``frame`` (accessibility bounds, already clipped to the
        window) has a non-zero size, its vertical centre is on screen, and it is
        above the pinned compose bar (``dm-text-field``) when that is shown.

        Compose's LazyColumn composes only the rows at or near the viewport, so an
        unrealized row has no frame at all (the bridge answers ``None`` -> not in
        view); a realized row is further tested against the compose bar, which
        pins to the bottom of the same column and would otherwise hide a bubble
        scrolled underneath it. Read-only, never scrolls — linux's, tui's and
        windows's rule in this app's geometry."""
        frame = self.get_attr(element_id, "frame", index, scope=scope)
        if not frame:
            return False
        _x, y, _w, h = (float(p) for p in frame.split(","))
        if h <= 0:
            return False
        centre = y + h / 2
        if centre < 0:
            return False
        bar = self.get_attr("dm-text-field", "frame")
        if bar:
            bar_y = float(bar.split(",")[1])
            if centre >= bar_y:
                return False
        return True

    #: Commit-on-Enter inputs: a ``click`` on one is the commit, the type-then-click
    #: idiom the shared action layer uses for every app (``actions/custody.py``
    #: ``set_held_budget``). A UiAutomator tap only focuses a Compose text field,
    #: so the driver presses the field's own commit gesture instead — a hardware
    #: Enter, which a single-line field performs as its IME action (pinned by
    #: ``DevicesRosterContentTest.aHardwareEnterCommitsTheBudgetToo``).
    COMMIT_ON_ENTER_INPUTS = frozenset({"custody-held-budget-input"})

    def click(self, element_id: str, index: int = 0, *, scope: str | None = None) -> None:
        if element_id in self.COMMIT_ON_ENTER_INPUTS and index == 0:
            self.press_key(element_id, "Enter", scope=scope)
            return
        super().click(element_id, index, scope=scope)

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
        the device, where no host path exists on any venue -- under emulator placement A2 the
        device is not even on this machine (`testing.md` § Default app and nest
        mode → *Android's emulator placement*). So the file's bytes ride the bridge's own
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
        from .port_util import find_free_port

        app_apk = config.get("app_path")
        test_apk = config.get("test_apk")
        self._device_serial = config.get("device_serial")
        self._adb_server = config.get("adb_server")
        self._device_port = config.get("bridge_port", 18500)

        # A venue device is a connect address: re-attach it before the first
        # adb call, in case its tunnel flapped since the probe (idempotent).
        from helpers import android_device

        android_device.connect_device(self._device_serial, self._adb_server)
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

        # Before the bridge starts, while nothing of the app is running: a first
        # launch finds no store an earlier, killed run left behind. A later
        # launch of this driver is the same machine, and keeps what it wrote.
        if config.get(self._PRODUCTION_STORE_KEY) and not getattr(self, "_launched_once", False):
            self._wipe_production_store(check=True)

        self._apply_permission_grants()
        self.set_app_locale(config.get("locale"))

        # ADB port-forward: local port -> device port. `adb forward` listens on
        # the machine running the adb SERVER, which is always this one: a local
        # server, or the android emulator's own server on this machine reaching the
        # emulator by `adb connect` (`helpers/android_emu.py`). So any free
        # port will do, in a venue run too.
        self._local_port = find_free_port()
        subprocess.run(
            [*adb, "forward", f"tcp:{self._local_port}", f"tcp:{self._device_port}"],
            check=True, capture_output=True,
        )

        # ADB REVERSE, the other direction: the DEVICE's own 127.0.0.1:<p> is
        # carried back to <p> on the machine running the adb server, where the
        # test nest listens. This is constraint 3 of testing.md § Default app and
        # nest mode → *Android's emulator placement*, and it is what lets the app be
        # handed the harness's ordinary `http://127.0.0.1:<port>` URL with no
        # rewriting anywhere -- the device's loopback IS the nest.
        #
        # The reverse lands on the adb SERVER's loopback, which is this machine
        # in every venue: a local emulator, a USB device, and the android emulator,
        # whose adb server runs here and reaches the emulator's adbd through the
        # tunnel (testing.md § *A2 amended — the design*, point 2).
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
        posted_at = time.time()
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
        self._wait_for_health(posted_at, timeout=self._RELAUNCH_READY_BUDGET_S)

        # Kept for `preserve_state_across_relaunch()`, which pins into it, and
        # for the relaunch `recover()` runs from it.
        self._launch_config = config
        self._note_store_owner(config)
        self._mark_log_baseline()
        self._post("/session", self._session_body(config))
        self._mark_app_launch()

    #: The `_launch_config` key `preserve_state_across_relaunch()` pins. android
    #: has no per-launch store dir to pin (the base vocabulary,
    #: `_RELAUNCH_PIN_KEYS`): the app's filesDir already survives every
    #: relaunch, and the one thing that would replace the store is the
    #: launch's own `seed_credentials` overwrite — so the pin is "don't seed".
    _PRESERVE_STORE_KEY = "preserve_store"

    #: The launch-config key of convention 12's Android arm: this launch rests its
    #: identity in the app's production store, `EncryptedSharedPreferences` under a
    #: Keystore master key. The app takes that store whenever the launch intent
    #: names no credential file, and the bridge names one whenever the file exists
    #: (`AppLauncher.launchApp`), so the mode is the file's absence: nothing seeds
    #: it, on any launch of this machine, and `_wipe_production_store` removes it.
    _PRODUCTION_STORE_KEY = "production_store"

    #: Everything a production-store launch owns on the device, relative to the
    #: app's data directory: the two encrypted stores (`SecretBackend.kt`'s
    #: `SECURE_PREFS_FILE` and `INSTALL_PREFS_FILE`) and the two e2e files whose
    #: presence would put the launch back on the file arm (`AppLauncher`'s
    #: `credentialFile()` and `LaunchModule`'s sibling `install-secrets.json`).
    _PRODUCTION_STORE_FILES = (
        "shared_prefs/fauna_secure_prefs.xml",
        "shared_prefs/fauna_install_prefs.xml",
        "files/e2e_credentials.json",
        "files/install-secrets.json",
    )

    def _wipe_production_store(self, *, check: bool) -> None:
        """Stop the app and remove `_PRODUCTION_STORE_FILES` — the borrow and the
        give-back of convention 12's Android arm. The app goes first: a live
        process keeps its SharedPreferences in memory and writes them back. The
        Keystore master key stays, inert until a new store is made under it.
        ``check=False`` is teardown's: it must not raise over a device that went
        away, and the next launch's wipe covers what this one could not reach."""
        adb = self._adb_cmd()
        package = "social.fauna.fauna"
        subprocess.run(
            [*adb, "shell", "am", "force-stop", package],
            check=check, capture_output=True, timeout=15,
        )
        subprocess.run(
            [*adb, "shell", "run-as", package, "rm", "-f", *self._PRODUCTION_STORE_FILES],
            check=check, capture_output=True, timeout=15,
        )

    def set_app_locale(self, tag: str | None) -> None:
        """Pin (or, with ``None``, clear) the app's per-app locale before it starts.

        The Android platform mechanism for a locale chosen per launch: the
        framework's per-app language setting (`cmd locale set-app-locales`,
        Android 13+), which is what `Locale.getDefault()` — and so
        `WeekFields.of(...)`, the source of the calendar's week start — reads
        inside the app process. A BCP-47 tag such as ``en-GB``. The setting
        persists per package, so a test that sets one clears it in its
        `finally` (``set_app_locale(None)``)."""
        adb = self._adb_cmd()
        subprocess.run(
            [*adb, "shell", "cmd", "locale", "set-app-locales", "social.fauna.fauna",
             "--locales", tag or ""],
            check=True, capture_output=True, timeout=15,
        )

    def _session_body(self, config: dict) -> dict:
        """The `/session` POST body that launches the app for `config`."""
        env = config.get("environment", {})
        env["FAUNA_E2E_BRIDGE"] = f"http://127.0.0.1:{self._device_port}"
        body = {
            "app": "social.fauna.fauna",
            "environment": env,
            # This driver's first launch is a new install: the bridge clears the
            # install-scoped state the other apps' per-test install dir starts
            # without (today the push intent file — `AppLauncher.launchApp`).
            # Every later launch of the same driver is the same install.
            "fresh_install": not getattr(self, "_launched_once", False),
        }
        self._launched_once = True
        if config.get(self._PRODUCTION_STORE_KEY):
            # Even an empty seed creates the file whose absence is this mode.
            # `AndroidCredStore` always sends one, so `{}` is dropped; a real seed
            # is an identity the test did not sign in through the wizard, whose
            # whole subject is where a sign-in rests it, so it is refused.
            if config.get("seed_credentials"):
                raise ValueError(
                    "a production_store launch takes no seed_credentials: the identity "
                    "must arrive through a real sign-in"
                )
            return body
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

        `recover()` relaunches from `_launch_config` via `_session_body`, so the
        pinned `_PRESERVE_STORE_KEY` withholds the seed overwrite and the new
        process reads back the store the old one wrote. Answers False before
        any launch (no config to pin) or if the driver ever stops relaunching.
        """
        config = getattr(self, "_launch_config", None)
        if config is None or not self.supports_cold_relaunch():
            return False
        config[self._PRESERVE_STORE_KEY] = True
        # So reset()'s `_clear_relaunch_pin` un-pins it, and `hard_reload` takes
        # its pinned contract (wait for the app's own auto-login).
        self._record_relaunch_pin(config, (self._PRESERVE_STORE_KEY,))
        return True

    #: Where an actor's account store rests inside the app's data directory:
    #: `AccountStores.accountStoreContainerDir()` (`files/account-store`) is the
    #: `StoreRoot` the app hands shared Rust, and
    #: `fauna_account_store::root::StoreRoot::store_dir` scopes
    #: `<actor>/account-store` under it.
    _DEVICE_STORE_ROOT = "files/account-store"

    def wipe_replica_at_next_relaunch(self, actor_id_hex: str) -> bool:
        """Make the next relaunch a machine whose local account store was wiped
        — android's arm of the base contract.

        android carries nothing host-side: the app's filesDir outlives every
        relaunch, so an unpinned relaunch keeps the replica on the device while
        its empty seed erases the credential file, and the replayed sign-in
        adopts the surviving replica under a fresh writer
        (`account-replica-posture.md` § The store device principal, refinement
        10). Withholding the replica is therefore a DELETE on the device:
        `recover()` removes `<store root>/<actor>/account-store` inside the app
        sandbox (`run-as`, the e2e build is debuggable) after the app is stopped
        and before it starts again, once. The relaunched seat then holds a
        replica that has never listed the account.

        False when the next relaunch would not be that machine: before any
        launch, under a `preserve_state_across_relaunch()` pin (its whole point
        is that the store survives), or on a production-store launch."""
        config = getattr(self, "_launch_config", None)
        if config is None or self._relaunch_pinned() or config.get(self._PRODUCTION_STORE_KEY):
            return False
        actor = actor_id_hex.lower()
        if len(actor) != 64 or any(c not in "0123456789abcdef" for c in actor):
            raise ValueError(f"not an actor id: {actor_id_hex!r}")
        self.__dict__.setdefault("_replica_wipes", set()).add(actor)
        return True

    def _wipe_device_replicas(self) -> None:
        """Consume `wipe_replica_at_next_relaunch`: remove each named actor's
        replica from the app's data directory. Called by `recover()` with the
        app stopped (a live process holds the store open). A refusal raises —
        a relaunch that silently kept the replica would pass the first-sync
        witness for the wrong reason."""
        wipes = self.__dict__.pop("_replica_wipes", set())
        for actor in sorted(wipes):
            rel = f"{self._DEVICE_STORE_ROOT}/{actor}/account-store"
            done = subprocess.run(
                [*self._adb_cmd(), "shell",
                 f'run-as social.fauna.fauna sh -c "rm -rf {rel} && test ! -e {rel} && echo FAUNA_WIPED"'],
                capture_output=True, text=True, timeout=30,
            )
            if "FAUNA_WIPED" not in (done.stdout or ""):
                raise RuntimeError(
                    f"the replica wipe of {rel} was refused on the device: "
                    f"rc={done.returncode} stdout={done.stdout!r} stderr={done.stderr!r}"
                )

    def recover(self) -> bool:
        """Relaunch the app under test in place — the windows shape.

        The instrumentation bridge, the adb forward (and its lease) and every
        recorded reverse stay up: only the app process is cycled, by the
        bridge's own `DELETE /session` (`stopApp`) then `POST /session`
        (`launchApp`, which itself `am force-stop`s first). Tearing the bridge
        down and relaunching would zero `_reverse_ports` and lose every port the
        tests added beyond the launch config.

        The relaunched app comes back SIGNED OUT, like every other driver's: the
        bridge forwards whatever credential file the previous process left in
        filesDir (`AppLauncher.launchApp`), so an unpinned relaunch whose config
        carries no `seed_credentials` posts an explicit EMPTY seed, which the
        bridge writes over that file. A pinned relaunch
        (`preserve_state_across_relaunch`) withholds the seed altogether via
        `_session_body`, so the new process reads back the store the old one wrote.

        The app it is about to close may already be dead of a crash — the `app`
        fixture reaches here when a reset was never acknowledged — so the exit is
        read first (convention 6's close rider), before the health wait: on
        android the instrumentation bridge lives in the app's own process, so a
        crash that killed the app killed the bridge with it, and the health wait
        below is what then gives up.
        """
        self._read_exit_before_close("the relaunch in recover()")
        if not self._await_health():
            return False
        config = getattr(self, "_launch_config", None)
        if config is None:
            # launch() was never called; nothing to relaunch
            self._bridge_dead = False
            return True
        self._bridge_dead = False
        try:
            self._delete("/session")
        except Exception:
            self._bridge_dead = True
            return False
        # The app is stopped (`stopApp` force-stops it): the one window in which
        # a replica can be removed without a live process holding it open.
        self._wipe_device_replicas()
        body = self._session_body(config)
        if not config.get(self._PRESERVE_STORE_KEY) and not config.get(self._PRODUCTION_STORE_KEY):
            # The empty seed below erases the store with every enrollment in it
            # still live nest-side (a relaunch signs nothing out): keep what it
            # held, so a later teardown can still retire it.
            self._vault_slots_before_erase()
            body.setdefault("seed_credentials", {})
        # Re-take the log baseline so the pinned "per-launch" scope stays true:
        # the relaunched process's log is read from here, not from the dead
        # instance's tail.
        self._mark_log_baseline()
        try:
            self._post("/session", body)
        except Exception:
            self._bridge_dead = True
            return False
        self._mark_app_launch()
        return True

    # --- A discarded driver retires its machine (convention 10) ---

    def _note_store_owner(self, config: dict) -> None:
        """Record, at each `launch()`, whose store this machine's is. A seeded
        launch is a store the CALLER wrote (`AndroidLaunchHarness`), whose
        relaunch is `teardown()` + an unseeded `launch()` resuming it — so once
        any launch of this driver was seeded, the flag stays set."""
        self._store_caller_owned = (
            getattr(self, "_store_caller_owned", False)
            or config.get("seed_credentials") is not None
        )

    def _discards_machine_at_teardown(self) -> bool:
        """True when this driver's teardown throws its machine away: no launch
        of it was seeded (a seeded store is the caller's, which a later launch
        resumes) and no `preserve_state_across_relaunch()` pin keeps it.

        android's store is the app's filesDir on the device, which outlives the
        driver; what makes a teardown a discard is that nothing this driver was
        asked to keep rests there. A later unseeded launch on the same device
        then finds the machine as its retirement left it — signed out."""
        return not getattr(self, "_store_caller_owned", False) and not self._relaunch_pinned()

    def _current_slots(self) -> dict[str, dict] | None:
        """The on-device credential file's account-store slots, read over the
        bridge (`_attached_slots`)."""
        return self._attached_slots()

    def _vault_slots_before_erase(self) -> None:
        """Fold the store's slots into the driver's vault before an unpinned
        relaunch's empty seed erases them — the base `_retire_discarded_machine`
        reads the vault for an actor an earlier launch of this machine held.
        Only on a machine the teardown will discard; never fails the relaunch."""
        if not self._discards_machine_at_teardown():
            return
        slots = self._current_slots()
        if slots:
            self.__dict__.setdefault("_principal_slot_vault", {}).update(slots)

    def quit_app(self) -> None:
        """The user quits the app and leaves the device as it is
        (`HttpBridgeDriver.quit_app`'s contract) — the bridge force-stops the app
        (`DELETE /session`, `AppLauncher.stopApp`) while the instrumentation
        bridge, the adb forward and every recorded reverse stay up, and the
        app's filesDir (its credential file and store) is untouched. So the
        later `hard_reload()` → `recover()` has a bridge to ask and a nest to
        reach. The inherited `_stop_app()` is the `teardown()` half here: it
        terminates the bridge and drops the forward and reverses, after which
        `recover()` finds no bridge and returns False."""
        if self._bridge_dead or not self._url:
            return
        self._read_exit_before_close("quit_app()")
        self._delete("/session")

    def enter_background(self) -> None:
        """Send the app to the background the way a user leaves a phone app —
        the HOME key, so Android delivers the real `ON_STOP` to the process
        lifecycle (`ConversationsManagerHost.onStop`, the leave door:
        `reserved-folders.md` § The leave-flush promise, "switching away on your
        phone").

        The app keeps RUNNING afterwards — backgrounding is not an exit — so
        assert the outcome by what the transition was meant to produce (the
        nest-side drafts blob), never by process death. The key goes through
        `adb shell input`, from outside the app, so nothing test-only sits
        between the user's gesture and the lifecycle callback; the iOS twin
        drives the delegate in-process only because `simctl` has no such verb
        (`IosDriver.enter_background`)."""
        subprocess.run(
            [*self._adb_cmd(), "shell", "input", "keyevent", "KEYCODE_HOME"],
            check=True, capture_output=True, timeout=15,
        )

    def _stop_app(self) -> None:
        self._read_exit_before_close("teardown")
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
        if self._widget_mirror:
            shutil.rmtree(self._widget_mirror, ignore_errors=True)
            self._widget_mirror = None
        config = getattr(self, "_launch_config", None) or {}
        if config.get(self._PRODUCTION_STORE_KEY) and not self._relaunch_pinned():
            self._wipe_production_store(check=False)

    def app_process_identity(self) -> str | None:
        """``"<pid>@<start ticks>"`` of the app's process, asked of the device's
        kernel — or ``None`` when the app has no process.

        The android stand-in for an out-of-process agent's pid: the app's
        background sync (`PhotoBackupEngine` over the in-process
        `FfiSyncEngineHost`) runs in the app's own process, so "the background
        sync was not restarted" is "the app's process was not restarted". The
        start time (`/proc/<pid>/stat` field 22, in clock ticks since boot) is
        what makes a recycled pid read as a different process."""
        text = self._device_shell(
            f'pid=$(pidof {self.APPLICATION_ID}); [ -n "$pid" ] && cat /proc/$pid/stat'
        )
        line = next((ln for ln in text.splitlines() if ")" in ln), "")
        if not line:
            return None
        pid = line.split(" ", 1)[0]
        # The comm field is parenthesized and may hold spaces: split after its
        # closing paren, where field 3 (state) is index 0, so field 22 is index 19.
        rest = line.rsplit(")", 1)[1].split()
        if not pid.isdigit() or len(rest) < 20:
            return None
        return f"{pid}@{rest[19]}"

    # --- Convention 6's close rider: was the app already dead of a crash? ---

    def _device_instant(self) -> tuple[float | None, str]:
        """The device's epoch clock and the app's pid, in one adb call."""
        return _device_instant_reading(
            self._device_shell(f'date +%s.%N; echo "pid $(pidof {self.APPLICATION_ID})"')
        )

    def _mark_app_launch(self) -> None:
        """Record, after each launch's `POST /session`, the device instant and
        the app's pid the next close reads its crash witnesses against: the pid
        is what narrows logcat to THIS app, and the instant to THIS launch. An
        unread device leaves the mark empty, and the close then reads nothing.
        Never raises: a launch that worked is not failed by its own witness."""
        try:
            at, pid = self._device_instant()
        except (OSError, subprocess.SubprocessError) as exc:
            print(f"[e2e] android: the launch's crash witness mark was not taken: "
                  f"{type(exc).__name__}: {exc}", flush=True)
            at, pid = None, ""
        self._app_launch_mark = None if at is None else (at, pid, time.time())

    def _read_exit_before_close(self, close: str) -> None:
        """Convention 6's close rider, the android leg: read whether the app
        had already died of a crash, BEFORE the close (`DELETE /session` is
        `am force-stop`, which ends the app however it was).

        The witnesses are the device's, over `adb`: the app's process — on
        android the instrumentation bridge runs inside it, so its pid is the
        one `_mark_app_launch` read and it changes only when the process dies
        — and logcat's crash blocks since the launch (`_crash_block`). A gone
        pid with a block is a crash, recorded with `helpers/app_deaths.py`; a
        gone pid with none is printed as UNKNOWN, the absence stated, and fails
        nothing (a crash is the one ending that fails). A live pid, no launch
        to read against, a close already made (the mark is spent at the first
        close of each launch, so the teardown after a `quit_app()` reads
        nothing) and an unanswering device stay silent. Never raises."""
        mark = getattr(self, "_app_launch_mark", None)
        self._app_launch_mark = None
        if mark is None:
            return
        launched_at, launch_pid, host_launched_at = mark
        try:
            now, pid = self._device_instant()
            if pid and (pid == launch_pid or not launch_pid):
                return
            logcat = self._device_shell("logcat -d -b main,crash -v epoch")
        except (OSError, subprocess.SubprocessError) as exc:
            print(f"[e2e] android: the app's exit before {close} was unread: "
                  f"adb did not answer ({type(exc).__name__}: {exc})", flush=True)
            return
        try:
            found = _crash_block(
                logcat, package=self.APPLICATION_ID, pid=launch_pid, since=launched_at
            )
            if found is None:
                print(
                    f"[e2e] android: the app's process (pid {launch_pid or 'unread'}) was "
                    f"gone before {close} closed it: UNKNOWN — logcat holds no crash block "
                    f"for it since its launch", flush=True,
                )
                return
            report = self._app_death_report(close, found, launched_at, now)
        except Exception as exc:  # noqa: BLE001 - a diagnosis must never break the close
            report = (
                f"the app died before {close} closed it (diagnostics unavailable: "
                f"{type(exc).__name__}: {exc})"
            )
        from helpers import app_deaths

        app_deaths.record(report, launch=f"{host_launched_at:.3f}")

    def _app_death_report(
        self, close: str, found: tuple[str, list[str], float], launched_at: float, now: float | None
    ) -> str:
        """The verdict first, then when it died on the device's clock, then the
        logcat block whole and the instrumentation bridge's output tail (which
        ends `Process crashed.` when the crash took the bridge with it)."""
        fatal, block, died_at = found
        when = f"it died +{max(0.0, died_at - launched_at):.0f} s after its launch"
        if now is not None:
            when += f", {max(0.0, now - died_at):.0f} s before this close"
        bridge_tail = list(getattr(self, "_bridge_log", []) or [])[-20:]
        parts = [
            f"the app died before {close} closed it: {fatal}",
            f"{when} (the device's clock). The close is `am force-stop`, so the app died "
            "on its own before it: during the last command the harness sent it (a reset, "
            "a teardown's sign-out) or after the test's last step.",
            "  --- logcat, the crash block ---",
            *block,
        ]
        if bridge_tail:
            parts += ["  --- the instrumentation bridge's output, its tail ---", *bridge_tail]
        return "\n".join(parts)

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

    #: Where the app publishes its home-screen widget snapshot, relative to its
    #: private data directory (`UnreadSnapshotStore.forApp`: `filesDir/widget`).
    _WIDGET_SNAPSHOT = "files/widget/unread.json"

    def widget_dir(self) -> str | None:
        """A host directory mirroring the app's widget snapshot, as of this call.

        The widget lives outside the app's automation surface, so the witness
        reads what the app published for it from OUTSIDE the app
        (`apps/common.md` § Home-screen widget → *Witnessing it*): the
        ``unread.json`` `WidgetUnreadPublisher` writes into the app's private
        ``files/widget/``, read through adb (``run-as`` — the e2e build is
        debuggable), never through the bridge that runs inside the app. The
        apple drivers name a host directory the app writes into; the android
        file is on the device, so — like ``download_dir()`` — the answer is a
        host mirror, refreshed by THIS call and nothing else: a wait must call
        ``widget_dir()`` inside its predicate (``helpers.home_screen_widget``
        does). No snapshot on the device → none in the mirror.
        """
        if self._widget_mirror is None:
            import tempfile

            self._widget_mirror = tempfile.mkdtemp(prefix="fauna-e2e-android-widget-")
        local = os.path.join(self._widget_mirror, os.path.basename(self._WIDGET_SNAPSHOT))
        result = subprocess.run(
            [*self._adb_cmd(), "exec-out", "run-as", "social.fauna.fauna",
             "cat", self._WIDGET_SNAPSHOT],
            capture_output=True, timeout=15,
        )
        body = result.stdout.strip()
        # `exec-out` does not reliably carry the remote exit status, and
        # `run-as`/`cat` print their refusal ("No such file …") on stdout on
        # some images: only a JSON object is a snapshot.
        if result.returncode == 0 and body.startswith(b"{"):
            tmp = f"{local}.tmp"
            with open(tmp, "wb") as fh:
                fh.write(body)
            os.replace(tmp, local)
        elif os.path.exists(local):
            os.remove(local)
        return self._widget_mirror

    #: A path under the app's private data directory that `app_data_exists`
    #: will ask about: relative, `/`-separated segments of letters, digits,
    #: `.`, `_` and `-`, never `..`. The path is spelled into a device shell
    #: command line, so nothing else may reach it.
    _APP_DATA_SEGMENT = re.compile(r"[A-Za-z0-9._-]+")

    def app_data_exists(self, rel_path: str) -> bool:
        """Whether ``rel_path`` exists under the app's private data directory
        (``/data/user/0/social.fauna.fauna``: ``files/…``, ``databases/…``).

        The account-scoped directories (`AccountStores.kt`: ``<filesDir>/<actor>``
        and ``<databases>/<actor>``) are on-device, where no host path reaches
        them on any venue, and the bridge runs inside the very app whose erase
        is under test. So the witness reads from OUTSIDE the app, the way
        ``widget_dir()`` does: ``run-as`` (the e2e build is debuggable) starts in
        the data directory. ``exec-out`` does not reliably carry the remote exit
        status, so the answer is a marker the device shell prints only on
        success.
        """
        parts = rel_path.split("/")
        if not parts or any(
            p in ("", ".", "..") or not self._APP_DATA_SEGMENT.fullmatch(p) for p in parts
        ):
            raise ValueError(f"not a plain relative app-data path: {rel_path!r}")
        result = subprocess.run(
            [*self._adb_cmd(), "exec-out",
             f'run-as social.fauna.fauna sh -c "test -e {rel_path} && echo FAUNA_PRESENT"'],
            capture_output=True, timeout=15,
        )
        return b"FAUNA_PRESENT" in result.stdout

    def app_data_listing(self, rel_dir: str = "files", max_depth: int = 4) -> list[str]:
        """Every path under ``rel_dir`` of the app's private data directory, to
        ``max_depth`` levels — a diagnostic for a store the harness cannot find
        ("nothing there" vs "somewhere else"), read from outside the app the way
        ``app_data_exists`` is. Empty when the directory is absent."""
        self._require_plain_app_path(rel_dir)
        result = subprocess.run(
            [*self._adb_cmd(), "exec-out", "run-as", "social.fauna.fauna",
             "find", rel_dir, "-maxdepth", str(int(max_depth))],
            capture_output=True, timeout=30,
        )
        return sorted(
            line for line in result.stdout.decode(errors="replace").splitlines()
            if line.startswith(rel_dir)
        )

    # --- On-device filesystem faults and readings (the residue journeys) ---
    #
    # The desktop residue witnesses (`test_sign_out_tui.py`) inject a sign-out
    # fault with host permission bits and read the store tree host-side. On
    # android the store is inside the app sandbox, so the same four acts run as
    # the app's own uid through `run-as` (the e2e build is debuggable), from
    # outside the app. Every path is checked by `_require_plain_app_path`
    # before it reaches the device shell, and `exec-out` carries no exit
    # status, so success is a marker the device shell prints only then.

    def _run_as_script(self, script: str, *, timeout: float = 30) -> bytes:
        result = subprocess.run(
            [*self._adb_cmd(), "exec-out", f'run-as social.fauna.fauna sh -c "{script}"'],
            capture_output=True, timeout=timeout,
        )
        return result.stdout

    def app_data_tree(self, rel_dir: str) -> dict[str, tuple[bool, int, str]]:
        """Every entry under ``rel_dir`` of the app's data directory: path
        relative to ``rel_dir`` → ``(is_dir, size, mtime)`` (mtime as the
        device's own ``stat %y`` text, nanoseconds included — compared, never
        parsed). Empty when the directory is absent or unreadable."""
        self._require_plain_app_path(rel_dir)
        out = self._run_as_script(
            f"find {rel_dir} -mindepth 1 -exec stat -c '%F|%s|%y|%n' {{}} +", timeout=60,
        )
        tree: dict[str, tuple[bool, int, str]] = {}
        prefix = rel_dir.rstrip("/") + "/"
        for line in out.decode(errors="replace").splitlines():
            parts = line.split("|", 3)
            if len(parts) != 4 or not parts[3].startswith(prefix):
                continue
            kind, size, mtime, name = parts
            with contextlib.suppress(ValueError):
                tree[name[len(prefix):]] = (kind == "directory", int(size), mtime)
        return tree

    def app_data_dir_modes(self, rel_dir: str) -> dict[str, int]:
        """``rel_dir`` and every directory under it → its permission bits."""
        self._require_plain_app_path(rel_dir)
        out = self._run_as_script(f"find {rel_dir} -type d -exec stat -c '%a|%n' {{}} +")
        modes: dict[str, int] = {}
        for line in out.decode(errors="replace").splitlines():
            mode, _, name = line.partition("|")
            if name == rel_dir or name.startswith(rel_dir.rstrip("/") + "/"):
                with contextlib.suppress(ValueError):
                    modes[name] = int(mode, 8)
        return modes

    def app_data_chmod(self, modes: dict[str, int]) -> None:
        """Set each app-data path's permission bits; raises when the device
        refused any of them (a fault that silently never landed would pass the
        journey for the wrong reason). Applied in the given order."""
        for rel in modes:
            self._require_plain_app_path(rel)
        for rel, mode in modes.items():
            out = self._run_as_script(f"chmod {mode:o} {rel} && echo FAUNA_CHMOD")
            if b"FAUNA_CHMOD" not in out:
                raise RuntimeError(f"run-as chmod {mode:o} {rel} was refused: {out[:300]!r}")

    def app_data_denies(self, rel_dir: str, *, listing: bool) -> bool:
        """Whether the app's own uid is really refused at ``rel_dir`` — its
        listing (``listing=True``) or a write into it — the probe that the
        injected fault is a fault and not a no-op."""
        self._require_plain_app_path(rel_dir)
        if listing:
            script = f"ls {rel_dir} >/dev/null 2>&1 && echo FAUNA_ALLOWED"
        else:
            probe = f"{rel_dir}/.fault-injection-probe"
            script = f"touch {probe} 2>/dev/null && rm -f {probe} && echo FAUNA_ALLOWED"
        return b"FAUNA_ALLOWED" not in self._run_as_script(script)

    def app_data_read(self, rel_path: str) -> bytes | None:
        """The bytes of one app-data file, or None when it is absent."""
        self._require_plain_app_path(rel_path)
        out = self._run_as_script(f"test -f {rel_path} && echo FAUNA_FILE && cat {rel_path}")
        marker = b"FAUNA_FILE\n"
        if not out.startswith(marker):
            return None
        return out[len(marker):]

    def app_data_remove(self, rel_paths) -> None:
        """Best-effort ``rm -rf`` of app-data paths — a journey's cleanup of
        what it left on a device every later test shares. Call it with the
        app stopped (a live process holds its stores open)."""
        rels = list(rel_paths)
        for rel in rels:
            self._require_plain_app_path(rel)
        if rels:
            self._run_as_script("rm -rf " + " ".join(rels))

    def pull_app_data(self, dest) -> Path:
        """The app's whole private data directory, as host files under ``dest``,
        for a search that reads every byte the app rests.

        Read from outside the app, the way ``app_data_exists`` is: ``run-as`` (the
        e2e build is debuggable) starts in the data directory, and ``exec-out`` is
        byte-clean. Regular files only, so nothing in the archive can point
        outside ``dest``. ``exec-out`` carries no exit status, so an answer that
        is no archive raises, never reading as a device with nothing on it.
        """
        result = subprocess.run(
            [*self._adb_cmd(), "exec-out", "run-as", "social.fauna.fauna",
             "tar", "-cf", "-", "."],
            capture_output=True, timeout=120,
        )
        root = Path(dest)
        root.mkdir(parents=True, exist_ok=True)
        try:
            with tarfile.open(fileobj=io.BytesIO(result.stdout), mode="r:") as archive:
                archive.extractall(
                    root,
                    filter=lambda member, path: (
                        tarfile.data_filter(member, path) if member.isreg() else None
                    ),
                )
        except tarfile.ReadError as exc:
            raise RuntimeError(
                f"run-as tar answered no archive ({exc}): "
                f"{result.stdout[:200]!r} {result.stderr[:200]!r}"
            ) from exc
        return root

    def custodian_store_blob_paths(self) -> list[str]:
        """Every sealed blob file the in-app custodian store holds, as paths
        relative to the app data directory.

        The store lives INSIDE the app sandbox — ``<filesDir>/<actor-hex>/
        backup-custodian/blobs/<2-hex>/<64-hex>`` (``AccountStores.custodianStoreBaseDir``
        + the shared ``custodian_store_root``) — where no host path reaches it, so
        the disk half of the orphaned-store witness reads it the way
        ``app_data_exists`` does: ``run-as`` (the e2e build is debuggable). Files
        only, like ``helpers.sync_agent_config.custodian_store_blobs``.
        """
        result = subprocess.run(
            [*self._adb_cmd(), "exec-out", "run-as", "social.fauna.fauna",
             "find", "files", "-type", "f", "-path", "*/backup-custodian/blobs/*"],
            capture_output=True, timeout=30,
        )
        return sorted(
            line for line in result.stdout.decode(errors="replace").splitlines()
            if "/backup-custodian/blobs/" in line
        )

    #: What `overwrite_custodian_store_blobs` writes and
    #: `custodian_store_blobs_holding` looks for — plain ASCII, so it rides a
    #: device shell command line unquoted.
    _CUSTODIAN_ROT = "rot"

    def overwrite_custodian_store_blobs(self) -> int:
        """Overwrite every sealed blob of the in-app custodian store with
        `_CUSTODIAN_ROT`, and return how many were written — the emulator twin
        of the host-side `blob.write_bytes(b"rot")` the store-rot witness does
        on a desktop store.

        An OVERWRITE, never a delete, for the witness's own reason (a delete
        fails a mere stat; an overwrite fails only a store that verifies the
        content it claims). Run inside the sandbox as the app's own uid
        (``run-as`` — the e2e build is debuggable), from outside the app the
        way ``custodian_store_blob_paths`` reads it. Blob names are hex
        (``<2-hex>/<64-hex>``), so the device shell needs no quoting for them;
        ``exec-out`` carries no exit status, so the count is a marker printed
        once per file written.
        """
        script = (
            "find files -type f -path '*/backup-custodian/blobs/*' | "
            f"while read f; do printf {self._CUSTODIAN_ROT} > \\$f && echo FAUNA_ROTTED; done"
        )
        result = subprocess.run(
            [*self._adb_cmd(), "exec-out", f'run-as social.fauna.fauna sh -c "{script}"'],
            capture_output=True, timeout=60,
        )
        return result.stdout.count(b"FAUNA_ROTTED")

    def custodian_store_blobs_holding_rot(self) -> int:
        """How many of the in-app custodian store's blobs still hold exactly the
        bytes `overwrite_custodian_store_blobs` wrote — the heal half of the
        store-rot witness (a repaired blob holds its sealed bytes again). Size
        and content both, so a real blob merely starting with the same three
        bytes never counts."""
        rot = self._CUSTODIAN_ROT
        script = (
            "find files -type f -path '*/backup-custodian/blobs/*' | "
            f"while read f; do [ \\$(wc -c < \\$f) -eq {len(rot)} ] && "
            f"[ \\\"\\$(cat \\$f)\\\" = {rot} ] && echo FAUNA_STILL_ROT; done; true"
        )
        result = subprocess.run(
            [*self._adb_cmd(), "exec-out", f'run-as social.fauna.fauna sh -c "{script}"'],
            capture_output=True, timeout=60,
        )
        return result.stdout.count(b"FAUNA_STILL_ROT")

    def _require_plain_app_path(self, rel_path: str) -> None:
        parts = rel_path.split("/")
        if not parts or any(
            p in ("", ".", "..") or not self._APP_DATA_SEGMENT.fullmatch(p) for p in parts
        ):
            raise ValueError(f"not a plain relative app-data path: {rel_path!r}")

    def grant_photos_access(self) -> None:
        """Pre-grant the photo library read permission before ``launch()``
        starts the app — the android twin of the iOS driver's method of the same
        name. Turning on photo backup (`photo-backup-enable-toggle`) asks for it
        through the system permission dialog (`FoldersScreen.kt`'s
        ``PhotoBackupSection``), which belongs to the permission controller, not
        the app, so no in-app driver reaches it; ``pm grant`` answers it up
        front, as a user who already allowed access would have. Applied by
        ``launch()`` right after the install, since a grant needs the package."""
        self._grant_photos = True

    def add_photo_to_library(self, host_path: str) -> None:
        """Put a photo in the emulator's gallery — the android twin of the iOS
        driver's `simctl addmedia` method of the same name. The file is pushed
        under ``Pictures`` and indexed with the media scanner, then PROVED to be
        in MediaStore by name: a scan that is accepted but never indexes the
        file would otherwise read, from inside the app, exactly like a broken
        ingest (the failure the iOS method's own proof exists to rule out)."""
        name = Path(host_path).name
        device_path = f"/sdcard/Pictures/{name}"
        for argv in (
            ["push", host_path, device_path],
            ["shell", "am", "broadcast", "-a",
             "android.intent.action.MEDIA_SCANNER_SCAN_FILE",
             "-d", f"file://{device_path}"],
        ):
            result = subprocess.run(
                [*self._adb_cmd(), *argv], capture_output=True, text=True, timeout=60,
            )
            if result.returncode != 0:
                raise RuntimeError(
                    f"adb {argv[0]} failed (rc={result.returncode}) for {host_path!r}: "
                    f"{result.stderr.strip()}"
                )
        deadline = time.monotonic() + 30
        while True:
            query = subprocess.run(
                [*self._adb_cmd(), "shell", "content", "query", "--uri",
                 "content://media/external/images/media", "--projection",
                 "_display_name", "--where", f"_display_name='{name}'"],
                capture_output=True, text=True, timeout=30,
            )
            if name in query.stdout:
                return
            if time.monotonic() > deadline:
                raise RuntimeError(
                    f"the media scanner accepted {device_path} but MediaStore never "
                    f"listed {name!r}; the gallery has no photo for the app to find. "
                    f"last query: {query.stdout.strip()!r} {query.stderr.strip()!r}"
                )
            time.sleep(0.5)

    def grant_notifications(self) -> None:
        """Pre-grant ``POST_NOTIFICATIONS`` (API 33+) before ``launch()``
        starts the app. The shell asks for it at launch and the push toggle at
        toggle-on (``PushNotificationsSection.kt``); both dialogs belong to the
        permission controller, which no in-app driver reaches, so ``pm grant``
        answers up front, as a user who already allowed notifications would
        have — the push journeys then run the toggle, not the prompt."""
        self._grant_notifications = True

    def _apply_permission_grants(self) -> None:
        grant_photos = getattr(self, "_grant_photos", False)
        grant_notifications = getattr(self, "_grant_notifications", False)
        if not (grant_photos or grant_notifications):
            return
        sdk = subprocess.run(
            [*self._adb_cmd(), "shell", "getprop", "ro.build.version.sdk"],
            check=True, capture_output=True, text=True, timeout=15,
        ).stdout.strip()
        api33 = sdk.isdigit() and int(sdk) >= 33
        permissions: tuple[str, ...] = ()
        if grant_photos:
            # The same split the screen's own request makes: the READ_MEDIA_*
            # pair from API 33, READ_EXTERNAL_STORAGE below it.
            permissions += (
                ("android.permission.READ_MEDIA_IMAGES", "android.permission.READ_MEDIA_VIDEO")
                if api33
                else ("android.permission.READ_EXTERNAL_STORAGE",)
            )
        if grant_notifications and api33:
            # Below API 33 notifications need no runtime grant.
            permissions += ("android.permission.POST_NOTIFICATIONS",)
        for permission in permissions:
            subprocess.run(
                [*self._adb_cmd(), "shell", "pm", "grant", "social.fauna.fauna", permission],
                check=True, capture_output=True, timeout=15,
            )

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
        subprocess.run(
            [*self._adb_cmd(), "reverse", f"tcp:{port}", f"tcp:{port}"],
            check=True, capture_output=True,
        )
        self._reverse_ports.append(port)

    def set_provider_base_urls(self, urls: dict[str, str]) -> None:
        """The shared redirect (`HttpBridgeDriver.set_provider_base_urls`), after
        a reverse for every loopback port it names.

        The urls point at a host-side fake (`fakes/fake_cloud.py`); on the device
        127.0.0.1 is the device's own loopback, so each such port needs the same
        `adb reverse` a nest port gets, or the provider verify dies on a bare
        connection error. A non-loopback host is reached over the device's own
        network and is left alone."""
        from urllib.parse import urlsplit

        for url in urls.values():
            parts = urlsplit(url)
            if parts.hostname in ("127.0.0.1", "localhost") and parts.port:
                self.ensure_reverse(parts.port)
        super().set_provider_base_urls(urls)

    #: The installed app's application id (`applicationId` in `app/build.gradle.kts`;
    #: the source namespace stays `com.fauna.app`).
    APPLICATION_ID = "social.fauna.fauna"

    def dumpsys_package(self) -> str:
        """``dumpsys package`` for the INSTALLED app, read from the device —
        the package manager's own reading of the manifest it installed (its
        ``flags=[ … ]`` line carries ``ALLOW_BACKUP`` when backup is allowed)."""
        return subprocess.run(
            [*self._adb_cmd(), "shell", "dumpsys", "package", self.APPLICATION_ID],
            check=True, capture_output=True, text=True, timeout=30,
        ).stdout

    def pull_installed_apk(self, dest_dir) -> Path:
        """Copy the INSTALLED app's base APK off the device into ``dest_dir``
        (``pm path`` names it) — the artifact the device runs, not the build
        output the harness installed from."""
        listing = subprocess.run(
            [*self._adb_cmd(), "shell", "pm", "path", self.APPLICATION_ID],
            check=True, capture_output=True, text=True, timeout=30,
        ).stdout
        paths = [
            line.removeprefix("package:").strip()
            for line in listing.splitlines() if line.startswith("package:")
        ]
        base = next((p for p in paths if p.endswith("/base.apk")), None)
        if base is None:
            raise RuntimeError(f"pm path {self.APPLICATION_ID} named no base.apk: {listing!r}")
        dest = Path(dest_dir) / "base.apk"
        subprocess.run(
            [*self._adb_cmd(), "pull", base, str(dest)],
            check=True, capture_output=True, timeout=120,
        )
        return dest

    def _adb_cmd(self) -> list[str]:
        # The one home of the prefix, shared with the availability probe: the
        # executable (PATHEXT-aware), this launch's adb server and its device.
        from helpers import android_device

        return android_device.adb_argv(self._device_serial, self._adb_server)

    def _wait_for_health(self, posted_at: float, timeout: float = 30.0):
        """The launch gate: the instrumentation bridge's `/health` answering.
        Both ends go through convention 6's launch-timeout builder — the bridge
        exiting is its FAULTED verdict, not a second grammar."""
        import urllib.request
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._bridge_proc and self._bridge_proc.poll() is not None:
                raise RuntimeError(self._launch_timeout_report(posted_at, timeout))
            try:
                resp = urllib.request.urlopen(f"{self._url}/health", timeout=2)
                if resp.status == 200:
                    return
            except Exception:
                pass
            time.sleep(0.5)
        raise TimeoutError(self._launch_timeout_report(posted_at, timeout))

    #: The launch gate's budget: the instrumentation bridge's `/health` answering
    #: after `am instrument`. 30 s, the figure the gate already carried; a deadline
    #: poll, so a green run never pays it (convention 14). Named as windows' twin
    #: is, and never load-aware (convention 6's launch-timeout rider, its part (3)).
    _RELAUNCH_READY_BUDGET_S = 30.0

    #: How long each `adb` read of a failure report may take: it runs inside a
    #: ``raise``, and an emulator too wedged to answer is itself a stated absence.
    _WITNESS_ADB_TIMEOUT_S = 10.0

    def _launch_timeout_report(self, posted_at: float, budget_s: float) -> str:
        """`launch_timeout_report` over this launch's witnesses. Never raises."""
        try:
            witnesses = self._launch_witnesses(posted_at, budget_s)
        except Exception as exc:  # noqa: BLE001 - must never mask the real failure
            return (
                f"launch timeout ({budget_s:.0f} s): UNKNOWN — "
                f"the witnesses could not be gathered: {type(exc).__name__}: {exc}"
            )
        return launch_timeout_report(witnesses)

    def _device_shell(self, script: str) -> str:
        return subprocess.run(
            [*self._adb_cmd(), "shell", script],
            capture_output=True, text=True, timeout=self._WITNESS_ADB_TIMEOUT_S,
        ).stdout

    def _launch_witnesses(self, posted_at: float, budget_s: float, machine=None) -> LaunchWitnesses:
        """(a) the instrumentation bridge's handle, and the app's CPU time read on
        the device (`/proc/<pid>/stat` over `adb`); (b) logcat's tail for the app's
        pid, and the bridge's drained output; (c) the timeline; (d) THIS machine's
        agent ledger — the emulator runs on the emulator host, so the machine witness is the
        harness side's, and the report says so."""
        deadline_at = time.time()
        rc = self._bridge_proc.poll() if self._bridge_proc else None
        alive = rc is None
        cpu_s = wall_s = wait_share = None
        cpu_unread, wait_basis, wait_unread, pid = "", "", "", ""
        try:
            reading = self._device_shell(
                f"pid=$(pidof {self.APPLICATION_ID}); echo \"pid $pid\"; getconf CLK_TCK; "
                "[ -n \"$pid\" ] && cat /proc/$pid/stat; cat /proc/uptime; "
                "[ -n \"$pid\" ] && cat /proc/$pid/task/*/schedstat"
            )
            pid, cpu_s, wall_s, cpu_unread = _device_cpu_reading(reading)
            wait_share, wait_basis, wait_unread = _device_wait_reading(reading, pid, wall_s)
        except (OSError, subprocess.SubprocessError) as exc:
            cpu_unread = wait_unread = f"adb did not answer ({type(exc).__name__}: {exc})"
        # logcat narrowed to the app's pid: unnarrowed, another app's crash would
        # read as this launch's fault, so without a pid it is printed, not judged.
        log_lines, notes = [], [
            "(d) is this harness machine's: the emulator runs on the emulator host, so its "
            "own load is not in that reading"
        ]
        try:
            logcat = self._device_shell(f"logcat -d -t 80{f' --pid={pid}' if pid else ''}")
            lines = [ln for ln in logcat.splitlines() if ln.strip()]
            if pid:
                log_lines = lines
            else:
                notes += ["  --- logcat, not narrowed to the app (it has no pid) ---", *lines]
        except (OSError, subprocess.SubprocessError) as exc:
            notes.append(f"(b) logcat unread: {type(exc).__name__}: {exc}")
        if machine is None:
            from helpers import launch_load

            machine = launch_load.machine_window(posted_at, deadline_at, apps=("android",))
        return LaunchWitnesses(
            budget_s=budget_s,
            posted_at=posted_at,
            deadline_at=deadline_at,
            process_started_at=None if wall_s is None else deadline_at - wall_s,
            alive=alive,
            exit_phrase="" if alive else f"the bridge instrumentation exited with code {rc}",
            cpu_s=cpu_s,
            cpu_wall_s=wall_s,
            cpu_unread=cpu_unread,
            cpu_wait_share=wait_share,
            cpu_wait_basis=wait_basis,
            cpu_wait_unread=wait_unread,
            log_lines=log_lines,
            stderr_lines=list(getattr(self, "_bridge_log", []) or []),
            machine=machine,
            notes=notes,
        )
