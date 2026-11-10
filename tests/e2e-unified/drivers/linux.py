from __future__ import annotations

import json
import os
import shlex
import shutil
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path

from .base import rolling_log_sizes, rolling_log_text
from .http_bridge import HttpBridgeDriver
from .local_launch import (
    descendant_named,
    local_launch_timeout_report,
    mark_exit_reported,
    read_exit_before_close,
)
from .port_util import (
    find_free_port,
    popen_group_kwargs,
    terminate_tree,
    track_process,
    untrack_process,
)
from .session_bus import PrivateSessionBus
from .x_display import sweep_stale_x_locks


XVFB_SERVER_ARGS = "-screen 0 1920x1440x24"
"""The throwaway display's geometry. ``xvfb-run``'s own default is 1280x1024,
shorter than the app's default window (1090x1210 under Xvfb, where no monitor
list exists for the app to clamp to): a popover anchored below y=1024 — the
⋯ menu of the third tall card in a feed — cannot be placed on that screen and
never maps, so a click on it "succeeds" and no menu ever shows."""


def _headless_render_cmd_env(
    cmd: list[str], env: dict[str, str],
) -> tuple[list[str], dict[str, str]]:
    """Wrap `cmd`/`env` to render fauna-desktop on a throwaway Xvfb display when
    headless. Pure (only `shutil.which` I/O) so it's unit-testable.

    The agent drives by test id, never screen coordinates, so a software
    framebuffer changes nothing functional — it only stops the app stealing
    focus on a real desktop
    session. `xvfb-run` sets `DISPLAY` to the Xvfb but leaves `WAYLAND_DISPLAY`
    untouched, and GTK4 prefers Wayland — so the app would pop onto the
    developer's screen. Force `GDK_BACKEND=x11` (GTK then only attempts x11,
    binding the Xvfb `DISPLAY`) and drop `WAYLAND_DISPLAY` as defense in depth.
    Do NOT drop `DISPLAY`: xvfb-run sets it to the Xvfb, and removing it would
    let x11 fall back to the visible `:0`. The cairo renderer avoids GTK4 GL
    init failing under the virtual framebuffer.

    Opt out with `FAUNA_E2E_REAL_DISPLAY=1` to watch the UI on the real display
    (also the fallback when xvfb-run isn't installed).
    """
    if not env.get("FAUNA_E2E_REAL_DISPLAY") and shutil.which("xvfb-run"):
        cmd = ["xvfb-run", "-a", "-s", XVFB_SERVER_ARGS, *cmd]
        env["GDK_BACKEND"] = "x11"
        env.pop("WAYLAND_DISPLAY", None)
        env.setdefault("GSK_RENDERER", "cairo")
    return cmd, env


def _xdg_runtime_dir(xdg_base: str) -> str:
    """The private `XDG_RUNTIME_DIR` a launch rooted at ``xdg_base`` gets. Pure.

    One home for the derivation `build_launch_env` sets and
    `LinuxBridgeDriver.sync_agent_socket` reads back."""
    return os.path.join(xdg_base, "runtime")


def build_launch_env(config: dict, port: int, tmp: str) -> dict:
    """The child environment for one isolated fauna-desktop launch.

    Fresh XDG dirs + a unique keyring namespace + a file-backed credential
    dir (headless boxes have no usable Secret Service), so the app boots
    clean and concurrent runs don't collide. Split out pure for the tier_1
    unit test — mirrors `drivers/tui.py`'s `build_launch_env`, which is the
    template (priorities #1/#3: two drivers doing one thing two ways).

    **`use_real_keyring` mode** reproduces the production path instead: a
    STABLE keyring namespace + STABLE XDG dirs (so the identity trio, the
    pending-invite resume slot, and the nest pin-store all survive a
    relaunch), and deliberately NO `FAUNA_E2E_CREDENTIAL_DIR` so every
    credential slot routes to the Secret Service on the launch's bus
    (`client.rs cred_file_dir()` is `None`). That service is a real
    `gnome-keyring-daemon` the test's `LibsecretCredStore` runs privately for
    it (`drivers/secret_service.py`), reached through the caller-owned bus in
    `config["environment"]` — never the desktop's keyring. Used by the
    launch-routing / nest-pin harness tests and the account-switcher abandon
    test. `tmp` in this mode IS the caller's `config["xdg_base"]` (no fresh
    scratch dir minted), and — unlike the isolated mode below — it does NOT
    isolate `XDG_RUNTIME_DIR`; that is an existing gap, unchanged by this
    lift, not a regression.
    """
    env = dict(os.environ)
    env.update(config.get("environment", {}))
    # A `RUST_LOG` the human exported survives a launch config that sets its own,
    # appended so both sets of directives apply — the same rule as tui's
    # `build_launch_env` and the windows driver's inheritance.
    inherited_log = os.environ.get("RUST_LOG")
    configured_log = config.get("environment", {}).get("RUST_LOG")
    if inherited_log and configured_log and inherited_log != configured_log:
        env["RUST_LOG"] = f"{configured_log},{inherited_log}"

    # The resident engines' full-reconcile cadence (`always_resident::rescan_interval`,
    # the compile-gated `FAUNA_E2E_RESCAN_MS` seam). Production ticks at the 300 s
    # constant (phase 5's de-knob — file-sync.md § Config, the phase-5 block); the
    # harness defaults every launch to 30 s — comfortably inside the 60 s budgets
    # the scan-driven tier_3 tests sized for the 60 s cadence the wizard's
    # frequency picker used to let them choose (a mass delete inotify never
    # itemizes, a missed watcher event — `test_filesync_mass_delete_floor.py`;
    # the `bound_location_media_app` fixture's disk-delete witness). A
    # caller-supplied override in `config["environment"]` (merged above) wins —
    # the debounce test pushes it PAST its budget instead.
    env.setdefault("FAUNA_E2E_RESCAN_MS", "30000")

    # The agent's e2e custody hold (`content_keys::custody_reread_held`, the
    # compile-gated `FAUNA_E2E_CUSTODY_HOLD_FILE` seam): a path in this launch's
    # scratch dir that does not exist, so the hold is inert until a test creates
    # the file (`helpers/folder_content.py::hold_custody_reread`) and released
    # when it is deleted. Read back through the driver's `custody_hold_file`.
    env.setdefault("FAUNA_E2E_CUSTODY_HOLD_FILE", os.path.join(tmp, "custody-hold"))

    if config.get("use_real_keyring"):
        xdg_data = os.path.join(tmp, "data")
        xdg_config = os.path.join(tmp, "config")
        os.makedirs(xdg_data, exist_ok=True)
        os.makedirs(xdg_config, exist_ok=True)
        env["XDG_DATA_HOME"] = xdg_data
        env["XDG_CONFIG_HOME"] = xdg_config
        env["FAUNA_KEYRING_APP"] = config["keyring_app"]
        env["FAUNA_E2E_AGENT_PORT"] = str(port)
        # Deliberately NO FAUNA_E2E_CREDENTIAL_DIR (libsecret is the sole
        # backend) and NO seed_credentials file (creds are injected into
        # libsecret or written by a real onboarding drive) — the caller
        # handles seeding, same as before this lift.
        return env

    # Per-app isolation: fresh XDG dirs + a unique keyring namespace so the
    # app boots clean (not on the real user's creds) and concurrent runs
    # don't collide. `keyring_app` / `credential_dir` / `xdg_base` pin each
    # to a stable, caller-supplied value instead — tui's `build_launch_env`
    # analogue for a launch-routing case that needs the identity persisted
    # across a relaunch without an unlocked session keyring (this mode's
    # file-backend-free alternative). Absent, each defaults to a fresh
    # per-launch value, so existing callers are unaffected.
    xdg_base = config.get("xdg_base") or tmp
    xdg_data = os.path.join(xdg_base, "data")
    xdg_config = os.path.join(xdg_base, "config")
    os.makedirs(xdg_data, exist_ok=True)
    os.makedirs(xdg_config, exist_ok=True)
    env["XDG_DATA_HOME"] = xdg_data
    env["XDG_CONFIG_HOME"] = xdg_config
    env["FAUNA_KEYRING_APP"] = config.get("keyring_app") or f"fauna-e2e-agent-{port}"

    # Headless boxes (and hardened deployments) have no usable Secret
    # Service — gnome-keyring's default collection is locked, so libsecret
    # writes fail with IsLocked and onboarding can't persist the identity.
    # Route the credential trio to a per-run private file store instead
    # (client.rs cred_file_dir / FAUNA_E2E_CREDENTIAL_DIR) so the real
    # onboarding flow persists + reloads creds without a keyring.
    creds_dir = config.get("credential_dir") or os.path.join(tmp, "creds")
    os.makedirs(creds_dir, exist_ok=True)
    env["FAUNA_E2E_CREDENTIAL_DIR"] = creds_dir
    env["FAUNA_E2E_AGENT_PORT"] = str(port)

    # Private XDG_RUNTIME_DIR (0700, per the basedir spec): the external
    # fauna-sync-agent's control socket lives at
    # `$XDG_RUNTIME_DIR/fauna/sync-agent.sock`, so inheriting the REAL
    # runtime dir would route every e2e launch — and the developer's own
    # desktop session — to ONE shared agent (testing.md § point 10's
    # machine-global leak class; same shape as the D-Bus bus fix). The
    # app direct-spawns the agent as a child under e2e, so both ends
    # inherit this private dir and the socket is per-launch by
    # construction.
    #
    # **A caller-passed `XDG_RUNTIME_DIR` OWNS it** — tui's twin carve-out
    # (`drivers/tui.py`), and the shape `DBUS_SESSION_BUS_ADDRESS` already takes
    # below. The account's conversations door sits beside the agent socket in
    # this dir (`conversations-oracle.md` § The custodian and the door), so a
    # second linux seat that must find the first one's door
    # (`test_cli_conversations_door.py`) names the first launch's dir. A caller
    # passes a PRIVATE dir (another launch's, or a `tmp_path` subdir); passing
    # the box's real runtime dir would re-open the machine-global agent-socket
    # leak this default closes — don't.
    xdg_runtime = config.get("environment", {}).get("XDG_RUNTIME_DIR") or _xdg_runtime_dir(
        xdg_base
    )
    os.makedirs(xdg_runtime, mode=0o700, exist_ok=True)
    env["XDG_RUNTIME_DIR"] = xdg_runtime

    return env


def _wants_private_bus(config: dict) -> bool:
    """Whether this launch should get its own D-Bus session bus. Pure.

    The display half above stops the app rendering onto the developer's screen;
    this stops the box's *desktop session* deciding the app's behavior. The bite
    is close-to-tray: `should_hide_to_tray()` is `CLOSE_TO_TRAY && TRAY_HOST_AVAILABLE`
    (`tray.rs:88`), a fresh e2e `XDG_CONFIG_HOME` has no `app-settings.json` so
    `CLOSE_TO_TRAY` falls back to its default (ON), and the tray
    host is simply "is `org.kde.StatusNotifierWatcher` on the bus we inherited".
    On a dev box gnome-shell owns that name and it comes and goes, so the same
    `window_close()` quit or hid by the hour and `wait_app_exit()` failed at random
    (diagnosed 2026-07-17). Default: a private bus owning nothing ⇒ no tray host ⇒
    close always quits, on every box.

    One carve-out: **the caller passed its own `DBUS_SESSION_BUS_ADDRESS`** — it
    is driving the bus deliberately, and the driver leaves it alone. Two callers
    do: `helpers/tray_bus.py` (`test_tray_close_to_tray.py` needs host-present
    *and* host-absent, so it cannot take a fixed default), and the
    `use_real_keyring` launches, whose bus carries the private Secret Service the
    test's `LibsecretCredStore` runs for them (`drivers/secret_service.py`).

    `use_real_keyring` used to be a second carve-out that kept the AMBIENT bus —
    "that mode's subject is the real Secret Service, which lives on the real
    session bus". It does need a real Secret Service implementation; it never
    needed the developer's. On that bus the force-quit tests crashed the desktop
    `gnome-keyring-daemon`, which restarted with the login keyring locked, taking the
    developer's stored credentials with it. The ambient bus is never the answer again:
    `_secret_service_bus_missing` refuses the mode without a caller-owned bus.
    """
    if (config.get("environment") or {}).get("DBUS_SESSION_BUS_ADDRESS"):
        return False
    return True


def _secret_service_bus_missing(config: dict) -> bool:
    """True when a `use_real_keyring` launch carries no caller-owned bus. Pure.

    That mode routes every credential slot to the Secret Service on the launch's
    bus (no file backend), so without a bus the test's private daemon lives on
    it has nothing to talk to. Neither fallback is acceptable — a private bus
    owning nothing makes the app fast-fail every read and the test fail
    obscurely; the ambient bus is the machine-wide hazard `_wants_private_bus`
    describes — so `launch()` refuses loudly instead.
    """
    if not config.get("use_real_keyring"):
        return False
    return not (config.get("environment") or {}).get("DBUS_SESSION_BUS_ADDRESS")


def _private_bus_env(env: dict, address: str) -> dict:
    """Point `env` at a private session bus. Pure.

    `GTK_A11Y=none` because the private bus does no service activation: GTK's
    a11y-bus lookup would just `ServiceUnknown`-warn, and the automation agent
    reads the widget tree directly rather than via AT-SPI. The `DBUS_STARTER_*`
    pair is dropped for the same reason `WAYLAND_DISPLAY` is above — leaving a
    second, stale pointer at the real bus invites something to follow it.
    """
    env["DBUS_SESSION_BUS_ADDRESS"] = address
    env["GTK_A11Y"] = "none"
    env.pop("DBUS_STARTER_ADDRESS", None)
    env.pop("DBUS_STARTER_BUS_TYPE", None)
    return env


# ---------------------------------------------------------------------------
# In-process automation agent
# ---------------------------------------------------------------------------
# The Linux app is driven by an automation HTTP server *inside* the
# fauna-desktop process (`apps/fauna-linux/src/automation/`), not by an external
# accessibility bridge. The driver launches fauna-desktop with a per-instance
# FAUNA_E2E_AGENT_PORT, isolated XDG dirs + keyring, and a private credential
# store, then talks straight to the app's own `/element/*` + `/app/*` HTTP
# surface (the standard `HttpBridgeDriver` contract). No shared a11y bus, so
# concurrent e2e runs don't contend. Design + migration history tracked
# internally.


class LinuxBridgeDriver(HttpBridgeDriver):
    """Linux E2E driver backed by the in-process automation agent.

    Each `launch()` spawns a fresh, fully-isolated fauna-desktop process and
    drives it over its in-app HTTP server. `teardown()` kills it; `recover()`
    relaunches a wedged one so a single bad test costs one test, not the rest of
    the session.
    """

    # linux's OS region setting is the POSIX locale's territory.
    REGION_SOURCE_KEY = "source_system_locale"

    def log_scope_across_relaunch(self) -> str:
        """``"per-launch"`` — each `launch()` mints a fresh tmp dir and reopens `app.err` inside it.

        See the base declaration for what each answer means and why it is
        declared rather than inferred; pinned per driver by
        `tests/test_module_relaunch.py`.
        """
        return "per-launch"

    def is_linux(self) -> bool:
        return True

    def get_clipboard_text(self) -> str | None:
        """The display clipboard's text, read in-process from GDK's clipboard
        object by the agent's GET /clipboard/text (`automation/agent.rs`
        ``clipboard_text``). ``None`` when it holds no text — the windows
        driver's contract; an agent refusal raises rather than reading empty."""
        reply = self._get("/clipboard/text")
        if "error" in reply:
            raise RuntimeError(f"linux clipboard read refused: {reply['error']}")
        return reply.get("text")

    # The in-process agent serves POST /element/scroll-into-view (a targeted
    # vadjustment scroll on the nearest ScrolledWindow ancestor), so
    # `scroll_to` / offscreen-element retries take the precise path.
    _supports_scroll_into_view = True

    # The shared in-process agent (`fauna-e2e-agent`) serves GET /element/texts,
    # so a whole column of same-id rows is one find instead of one per row. On
    # GTK every per-row read walks the whole widget tree, so a full admin Logs
    # page (thousands of `log-entry` rows) took longer to read once than the
    # test's wait budget. /element/attrs is not served: its 404 keeps the loop.
    _supports_bulk_reads = True

    # This launch's private D-Bus session bus (`_wants_private_bus`), stopped by
    # `teardown()`. `None` = not launched yet, or this launch keeps the ambient bus.
    _session_bus: PrivateSessionBus | None = None

    # Maximum time to wait for a post-click child to appear when
    # `wait_for_child=` is supplied. Generous — most renders happen well
    # under 200ms; the ceiling is for the rare slow-machine case.
    _WAIT_FOR_CHILD_MAX_S = 1.5

    # Baseline post-click settle when no `wait_for_child=` is given. The agent
    # actuates synchronously on the GTK main loop, but the resulting layout pass
    # (show/hide, append/remove, page rebuild) is asynchronous, so a caller that
    # immediately asserts `is_visible(...)` can race it. 0.5s is the empirical
    # floor that lets a wizard `gtk::Stack` transition settle before the next
    # assertion (0.2s breaks test_handle_entry_outcomes::test_*_routes_to_invite).
    # Callers that know the successor element should pass `wait_for_child=` to
    # bypass this and wait exactly as long as the click handler needs.
    _DEFAULT_POST_CLICK_SLEEP_S = 0.5

    def click(self, element_id: str, index: int = 0, *,
              scope: str | None = None,
              wait_for_child: str | None = None) -> None:
        """Click an element, then settle for the async post-click render.

        - `wait_for_child=<id>`: poll for that child to be visible with a
          `_WAIT_FOR_CHILD_MAX_S` ceiling. Use when the test knows which element
          should appear after the click; gives the handler exactly the time it
          needs and no more.
        - default (no kwarg): briefly sleep `_DEFAULT_POST_CLICK_SLEEP_S` so the
          next assertion finds a settled tree. Use only when the test has no
          obvious successor to wait for.

        Either way, callers that need stronger guarantees should follow with an
        explicit `wait_for(...)` — the polling there is the authoritative wait.
        """
        super().click(element_id, index, scope=scope)
        if wait_for_child is not None:
            self.wait_for(wait_for_child, timeout=self._WAIT_FOR_CHILD_MAX_S)
            return
        time.sleep(self._DEFAULT_POST_CLICK_SLEEP_S)

    def in_viewport(self, element_id: str, *, index: int = 0,
                    scope: str | None = None) -> bool:
        """The agent's ``in-viewport`` attribute (`automation/agent.rs`): true
        when the widget's vertical centre lies inside its nearest
        ``gtk::ScrolledWindow``'s visible band. GTK realizes every row of a
        ``gtk::Box``, so the element registry alone cannot say — a row scrolled
        far out of view still counts and reads visible."""
        value = self.get_attr(element_id, "in-viewport", index, scope=scope)
        if value is None:
            raise AssertionError(
                f"linux answered no in-viewport for {element_id!r}[{index}] — the "
                "element is missing or the agent predates the attribute; "
                f"{self.diagnose(element_id, scope=scope)}"
            )
        return value == "true"

    # ----------------------------------------------------------------------
    # Window-level ops (close-to-tray / Track 4)
    # ----------------------------------------------------------------------
    def window_close(self) -> None:
        """Simulate the titlebar close button — runs the real
        `connect_close_request` handler so close-to-tray's hide-vs-quit decision
        is exercised end-to-end. Tolerates a dropped connection: when the handler
        quits the app, the HTTP reply races process exit. Assert the outcome via
        `is_app_alive()` / `wait_app_exit()`.
        """
        try:
            self._post("/window/close")
        except Exception:
            # A quit tears the agent down mid-reply — expected for the no-host /
            # close-to-tray-off path. Not an error; the caller checks liveness.
            pass

    def top_level_windows(self) -> list[dict]:
        """The X server's own list of this app's top-level windows — one
        ``{"title": str, "is_offscreen": bool}`` per window, empty when the app
        owns none. The windows driver's twin (``drivers/windows.py``), shaped the
        same so case J's ``_visible_windows`` reads both.

        The corroborating half of the ``--autostart`` tray-residency assertion
        (`apps/linux.md` § Auto-start at sign-in): the app publishes its own
        decision into ``state["launch"]``; this asks the display server, not the
        app, whether a window is mapped. It reads the launch's Xvfb display (the
        ``DISPLAY``/``XAUTHORITY`` ``xvfb-run`` handed ``fauna-desktop``, out of its
        ``/proc`` environ) and keeps the root window's children whose
        ``_NET_WM_PID`` is the app's own pid — Xvfb runs no window manager, so
        GTK's toplevels are the root's direct children. ``is_offscreen`` is "not
        ``IsViewable``". A launch on a real (possibly Wayland) display has no such
        query, so it raises rather than answer "no windows".
        """
        proc = getattr(self, "_app_proc", None)
        if proc is None or proc.poll() is not None:
            return []
        pid = descendant_named(proc.pid, "fauna-desktop")
        if pid is None:
            return []
        raw = Path(f"/proc/{pid}/environ").read_bytes().decode(errors="replace")
        environ = dict(kv.split("=", 1) for kv in raw.split("\0") if "=" in kv)
        display = environ.get("DISPLAY")
        if not display or environ.get("GDK_BACKEND") != "x11":
            raise RuntimeError(
                "top_level_windows needs the launch's headless Xvfb display (x11); this "
                f"launch has DISPLAY={display!r} GDK_BACKEND={environ.get('GDK_BACKEND')!r}"
            )
        x_env = {**os.environ, "DISPLAY": display}
        if environ.get("XAUTHORITY"):
            x_env["XAUTHORITY"] = environ["XAUTHORITY"]

        def x(*argv: str) -> str:
            return subprocess.run(
                argv, env=x_env, capture_output=True, text=True, timeout=10, check=True
            ).stdout

        windows = []
        for line in x("xwininfo", "-root", "-children").splitlines():
            wid = line.strip().split(None, 1)[0] if line.strip() else ""
            if not wid.startswith("0x"):
                continue
            try:
                wm_pid = x("xprop", "-id", wid, "_NET_WM_PID")
                info = x("xwininfo", "-id", wid)
            except subprocess.CalledProcessError:
                continue  # destroyed since the listing
            if wm_pid.rsplit("=", 1)[-1].strip() != str(pid):
                continue
            # `xwininfo: Window id: 0x… "Title"` — the title is the quoted tail.
            head = next((ln for ln in info.splitlines() if "Window id:" in ln), "")
            title = head.split('"', 1)[1].rsplit('"', 1)[0] if head.count('"') >= 2 else ""
            windows.append({"title": title, "is_offscreen": "Map State: IsViewable" not in info})
        return windows

    def is_app_alive(self) -> bool:
        """True while the launched fauna-desktop process is still running."""
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

    @property
    def sync_agent_socket(self) -> str | None:
        """The unix socket THIS launch's agent serves:
        `$XDG_RUNTIME_DIR/fauna/sync-agent.sock` under the private runtime dir
        `build_launch_env` made (`fauna_ipc::unix_transport::linux_socket_path`;
        the app direct-spawns the agent under e2e, so it inherits that dir) —
        `None` before launch. tui's twin is `drivers/tui.py`'s."""
        runtime = getattr(self, "_xdg_runtime", None)
        if not runtime:
            return None
        return os.path.join(runtime, "fauna", "sync-agent.sock")

    @property
    def config_home(self) -> str:
        """The per-launch `XDG_CONFIG_HOME` this instance was isolated into.

        The Linux app persists device-local state under `<config_home>/fauna`
        — notably the Settings → Sync location map at
        `<config_home>/fauna/sync/location-map.json` (sync.rs `config_dir()` /
        `sync_state_dir()`). A test asserting an on-disk side effect of a UI
        action reads it from here. Raises if accessed before `launch()`.
        """
        cfg = getattr(self, "_xdg_config", None)
        if cfg is None:
            raise RuntimeError("config_home accessed before launch()")
        return cfg

    def custody_hold_file(self) -> str | None:
        """The file whose existence holds this launch's agent from re-reading
        custody (`FAUNA_E2E_CUSTODY_HOLD_FILE`, set by `build_launch_env`);
        None before `launch()`. Driven through
        `helpers/folder_content.py::hold_custody_reread`."""
        return getattr(self, "_custody_hold_file", None)

    def download_dir(self) -> str | None:
        """Where fauna-desktop saves e2e-mode downloads (no save dialog under e2e).

        Mirrors the app-side seam (`file_list.rs`'s `FAUNA_E2E_DOWNLOAD_DIR`
        bypass on `snapshot-file-download-button`): unlike windows (which
        falls back to a fixed `%LocalAppData%` path the C# side and this
        driver must each independently know), `launch()` always sets
        `FAUNA_E2E_DOWNLOAD_DIR` in the child env — defaulting to a
        subdirectory of the per-launch scratch dir, unless the caller already
        supplied an override via `config["environment"]` — so this just reads
        back what the child process actually got. Returns `None` before
        `launch()`.
        """
        return getattr(self, "_download_dir", None)

    # ----------------------------------------------------------------------
    # Process lifecycle
    # ----------------------------------------------------------------------
    def launch(self, config: dict) -> None:
        self._launch_config = config
        port = find_free_port()
        self._agent_port = port
        self._url = f"http://127.0.0.1:{port}"

        # `use_real_keyring` (launch-routing / onboarding *smoke* mode, real
        # libsecret persistence — see `build_launch_env`'s docstring) is the
        # one caller that supplies its own scratch dir directly rather than
        # getting a fresh one minted: the caller supplies a unique stable
        # `keyring_app` + `xdg_base`, owns the bus carrying its private Secret
        # Service, and is responsible for sweeping the `<keyring_app>*` items
        # afterwards (`tests/common/cred_store.py::LibsecretCredStore`).
        if _secret_service_bus_missing(config):
            raise RuntimeError(
                "use_real_keyring launch without a caller-owned bus: that mode reads "
                "every credential from the Secret Service on the launch's bus, and the "
                "desktop keyring is never it (its daemon crashes on a force-quit and "
                "takes the machine's git credentials with it). Launch through "
                "`LibsecretCredStore.launch_config()`, which runs a private "
                "gnome-keyring-daemon and passes its bus in `environment`."
            )
        if config.get("use_real_keyring"):
            tmp = config["xdg_base"]
        else:
            tmp = tempfile.mkdtemp(prefix="fauna-e2e-linux-agent-")
        self._tmp_dir = tmp

        env = build_launch_env(config, port, tmp)
        # The store principal survives a relaunch (convention 10's carve-out,
        # `HttpBridgeDriver._begin_principal_slot_carry`): harvest the launch being
        # replaced BEFORE the `_resolved_*` store attrs below move to this one.
        self._begin_principal_slot_carry(config, env)
        # The per-launch `XDG_CONFIG_HOME` this instance was isolated into —
        # the device-local state dir the `config_home` property exposes.
        self._xdg_config = env["XDG_CONFIG_HOME"]
        # The runtime dir this launch's agent socket (and the conversations
        # door) derive from — caller-owned or the private default; see
        # `sync_agent_socket`.
        self._xdg_runtime = env["XDG_RUNTIME_DIR"]
        # The unified account-store root THIS launch resolved — shared Rust's
        # `production_base()` on linux is `<XDG_CONFIG_HOME>/fauna/sync`
        # (`fauna_account_store::root`) — published after the carry began, so
        # the harvest above read the launch being replaced; the carry restores
        # the signing-in actor's replica under it beside the slot.
        self._resolved_store_root = os.path.join(self._xdg_config, "fauna", "sync")
        # …and the install device secret the named sync row's id derives from,
        # laid down now, before the app starts (it names no account). linux's
        # install dir is the flat `<XDG_CONFIG_HOME>/fauna/sync`
        # (`sync.rs::flat_sync_dir`) — the same dir as the store root above.
        self._carry_install_device_secret(os.path.join(self._xdg_config, "fauna", "sync"))

        # Remember where this launch's client-local store landed, so
        # `preserve_state_across_relaunch()` can pin it if the test asks. Not
        # pinned by default: the harness's contract is that a relaunched
        # native app comes back with a FRESH store and the driver replays the
        # session (see `HttpBridgeDriver.hard_reload`), and several tests rely
        # on it — the mid-claim crash journey, for one, expects the relaunched
        # app to show the create-identity screen. Under `use_real_keyring`
        # this is what lets `preserve_state_across_relaunch()` work there too
        # (e.g. `test_nest_identity_pin.py`'s native "changed" arm) — its
        # caller-supplied STABLE `xdg_base`/`keyring_app` already reuse
        # themselves across a relaunch by construction, but the check only
        # sees that if these attrs are set.
        self._resolved_xdg_base = config.get("xdg_base") or tmp
        self._resolved_credential_dir = env.get("FAUNA_E2E_CREDENTIAL_DIR")
        self._resolved_keyring_app = env["FAUNA_KEYRING_APP"]

        # Optional: pre-seed the file-backed credential store BEFORE the app
        # boots, so the launch machine hydrates an identity + saved nest_url and
        # runs the silent challenge at launch (the real authenticated-relaunch
        # path) instead of showing the onboarding wizard. The client reads
        # `{FAUNA_E2E_CREDENTIAL_DIR}/{FAUNA_KEYRING_APP}.json` as the per-app
        # namespace (client.rs `cred_file_path` / `cred_app`); the map keys are
        # the identity trio `secret_key` / `node_url` / `device_id`. Used by the
        # version-mismatch tier_3 test to launch a client already pointed at a
        # degraded nest. Production never sets this (no env var → libsecret).
        # Skipped under `use_real_keyring` (no credential dir resolved there —
        # creds are injected into libsecret or written by a real onboarding
        # drive instead).
        #
        # The map is written verbatim, and `CredentialStore` (File backend)
        # maps every logical key straight to the file key, so a test seeds the
        # `AccountRegistry` state the app reads (the `fauna/index` blob +
        # per-actor `fauna/{actor}/{secret,nest_url,device_id}` slots) here —
        # build it with `common.accounts.build_registry_seed` /
        # `single_account_seed`. There is no single-slot shape any more.
        seed = config.get("seed_credentials")
        if seed and self._resolved_credential_dir:
            cred_file = os.path.join(
                self._resolved_credential_dir, f"{self._resolved_keyring_app}.json"
            )
            with open(cred_file, "w") as f:
                json.dump(dict(seed), f)
            os.chmod(cred_file, 0o600)

        # Default download dir for the `snapshot-file-download-button` e2e
        # bypass (see `download_dir()`) — a caller-supplied override in
        # `config["environment"]` already landed in `env` above and wins here.
        env.setdefault("FAUNA_E2E_DOWNLOAD_DIR", os.path.join(tmp, "e2e-downloads"))
        os.makedirs(env["FAUNA_E2E_DOWNLOAD_DIR"], exist_ok=True)
        self._download_dir = env["FAUNA_E2E_DOWNLOAD_DIR"]
        self._custody_hold_file = env.get("FAUNA_E2E_CUSTODY_HOLD_FILE")

        # Isolate from the box's desktop session bus (see `_wants_private_bus`).
        # `teardown()` stops the bus, so a `recover()`/`hard_reload()` relaunch
        # builds a fresh one — identical by construction (an empty bus owning
        # nothing), so nothing observable rides on the daemon's identity. A test
        # that needs a name to SURVIVE a relaunch owns its bus externally and
        # passes the address in (`helpers/tray_bus.py`), which this branch skips.
        if _wants_private_bus(config):
            if self._session_bus is None:
                self._session_bus = PrivateSessionBus().start()
            env = _private_bus_env(env, self._session_bus.address)

        cmd = [config["app_path"], *shlex.split(config.get("app_args", "") or "")]
        cmd, env = _headless_render_cmd_env(cmd, env)
        if cmd[:1] == ["xvfb-run"]:
            # Box hygiene before the throwaway display: a SIGKILLed Xvfb leaves
            # its /tmp/.X<n>-lock behind and `xvfb-run -a` climbs past it forever
            # — into the GDM greeter's 1024+ range (`drivers/x_display.py`).
            sweep_stale_x_locks()

        # Log to files (not PIPE) so a chatty app can't deadlock on a full pipe.
        self._app_stdout = open(os.path.join(tmp, "app.out"), "w")
        self._app_stderr = open(os.path.join(tmp, "app.err"), "w")
        # Group-leader + die-with-parent (PDEATHSIG on Linux): the xvfb-run +
        # fauna-desktop pair dies with this run even on `kill -9` of pytest.
        posted_at = time.time()
        self._app_proc = subprocess.Popen(
            cmd,
            env=env,
            stdout=self._app_stdout,
            stderr=self._app_stderr,
            **popen_group_kwargs(),
        )
        track_process(self._app_proc)
        # What the close rider's read needs: when this launch began, and the
        # wrapper the handle leads with, whose app it looks for under it.
        self._app_launched_at = posted_at
        self._app_wrapper = "xvfb-run" if cmd[:1] == ["xvfb-run"] else None

        # Both ends of the gate go through convention 6's launch-timeout builder:
        # an early exit is its FAULTED verdict, not a second grammar.
        deadline = time.monotonic() + self._RELAUNCH_READY_BUDGET_S
        while time.monotonic() < deadline:
            if self._app_proc.poll() is not None:
                # The FAULTED verdict carries this exit; the close after it is silent.
                mark_exit_reported(self._app_proc)
                raise RuntimeError(self._launch_timeout_report(posted_at))
            if self._agent_health_ok():
                self._bridge_dead = False
                return
            time.sleep(0.3)
        raise TimeoutError(self._launch_timeout_report(posted_at))

    #: The launch gate's budget: the agent's `/health` answering after the spawn.
    #: 30 s, the figure the gate already carried; a deadline poll, so a green run
    #: never pays it (convention 14). Named as windows' twin is, and never
    #: load-aware (convention 6's launch-timeout rider, its part (3)).
    _RELAUNCH_READY_BUDGET_S = 30.0

    def _launch_timeout_report(self, posted_at: float) -> str:
        """Convention 6's launch-timeout report over this launch's witnesses: the
        handle on `xvfb-run` (or the app), `fauna-desktop`'s own pid under it, the
        launch's `app.err`, its XDG log and this machine's agent ledger."""
        tmp = getattr(self, "_tmp_dir", None)
        return local_launch_timeout_report(
            proc=self._app_proc,
            comm="fauna-desktop",
            budget_s=self._RELAUNCH_READY_BUDGET_S,
            posted_at=posted_at,
            stderr_paths=(os.path.join(tmp, "app.err"),) if tmp else (),
            log_text=self.on_device_log_text(),
            apps=("linux",),
        )

    def _agent_health_ok(self) -> bool:
        try:
            with urllib.request.urlopen(f"{self._url}/health", timeout=1) as r:
                return r.status == 200
        except Exception:
            return False

    def app_stderr_text(self) -> str:
        """This launch's `app.err` so far ("" if it has none yet).

        The app logs the decisions that shape a close: the `[tray]` verdict line
        says whether it found a `StatusNotifierWatcher`, i.e. whether closing the
        window will quit or hide. Reading it is what turns "the app didn't exit"
        from a mystery into an answer — put it in the assertion message rather
        than debugging by screenshot (`testing.md` § Cross-app e2e conventions,
        point 6).
        """
        tmp = getattr(self, "_tmp_dir", None)
        if not tmp:
            return ""
        try:
            return Path(os.path.join(tmp, "app.err")).read_text(errors="replace")
        except OSError:
            return ""

    def on_device_log_text(self) -> str:
        """The app's own on-device rolling log file(s) — `fauna.log.<date>` under
        `<config>/fauna/logs/`, written by `install_logging()`'s `fauna_log::init`
        — or "". Not `app_stderr_text()`: that is the captured `app.err` stream,
        which proves nothing about the file (`observability.md` § 2. Persistence
        & privacy). Not named `app_log_text` either: `helpers/app_log_section.py`
        prefers that attribute, and the file lacks the non-tracing stderr a
        failure report wants."""
        cfg = getattr(self, "_xdg_config", None)
        return rolling_log_text(os.path.join(cfg, "fauna")) if cfg else ""

    def on_device_log_sizes(self) -> dict[str, int]:
        """Each on-device `fauna.log.*` file's size by name — the sizes half of
        `on_device_log_text()`, read from the same directory."""
        cfg = getattr(self, "_xdg_config", None)
        return rolling_log_sizes(os.path.join(cfg, "fauna")) if cfg else {}

    def inherited_agent_log_text(self) -> str:
        """The child sync agent's own rolling log (`<config>/fauna/sync/logs`):
        its lines are interleaved in `app.err`, which it inherits."""
        tmp = getattr(self, "_tmp_dir", None)
        return rolling_log_text(os.path.join(tmp, "config", "fauna", "sync")) if tmp else ""

    def ack_timeout_diagnostics(self, tail_lines: int = 40) -> str:
        """The tail of this launch's `app.err`, for an ack-timeout message.

        linux `block_on`s the `real_*` agent commands' nest round-trips on the
        GTK main thread, so a wedged round-trip stops the app acking anything —
        and the only witness is what it logged just before. See the base
        method's docstring for why this is worth carrying into the message.
        """
        text = self.app_stderr_text()
        if not text:
            return ""
        tail = "\n".join(text.splitlines()[-tail_lines:])
        return f"\n--- app.err (last {tail_lines} lines) ---\n{tail}"

    def _read_exit_before_close(self, close: str) -> None:
        """Convention 6's close rider over this launch's handle: `xvfb-run` (or the
        app), `fauna-desktop`'s own pid under it while the wrapper lives, and, on a
        crash, the launch's `app.err` and XDG log read before the close reclaims
        them (`drivers/local_launch.py::read_exit_before_close`)."""
        read_exit_before_close(
            getattr(self, "_app_proc", None),
            close=close,
            comm="fauna-desktop",
            launched_at=getattr(self, "_app_launched_at", None),
            wrapper=getattr(self, "_app_wrapper", None),
            stderr_text=self.app_stderr_text,
            log_text=self.on_device_log_text,
        )

    def _stop_app(self) -> None:
        """Kill the app process, its private bus, and close its log files."""
        self._read_exit_before_close("teardown")
        proc = getattr(self, "_app_proc", None)
        if proc is not None:
            # Group-wide TERM→KILL sweep (xvfb-run + fauna-desktop + any helper).
            terminate_tree(proc)
            untrack_process(proc)
            self._app_proc = None
        # After the app: the bus outliving its client is harmless, the reverse
        # leaves the app talking to a dead socket during its own shutdown.
        bus = self._session_bus
        if bus is not None:
            self._session_bus = None
            try:
                bus.stop()
            except Exception:
                pass
        for fp in (getattr(self, "_app_stdout", None), getattr(self, "_app_stderr", None)):
            try:
                fp and fp.close()
            except Exception:
                pass

    def preserve_state_across_relaunch(self) -> bool:
        """Pin this launch's client-local store so a later `recover()` /
        `hard_reload()` relaunch reads back what this process wrote.

        Off by default (see `launch`): each launch normally gets a fresh
        `mkdtemp` XDG base + credential dir, and a libsecret namespace derived
        from the agent *port* — which changes on relaunch. So client-side durable
        state is normally thrown away by the harness, which is fine for journeys
        whose recovery lives nest-side, and load-bearing for the mid-claim journey
        (it wants the relaunched app to show create-identity).

        A test that is specifically asserting *client-side* durability calls this
        first. The pending-factory-reset slot (gap CR-1, `common.md` § Client-state
        recoverability) is the case it exists for: the claim code is minted and
        persisted before the reset is dispatched, and if the harness discarded the
        store on relaunch the test would "prove" a resume no real user gets.

        The dirs already exist (this launch made them) and teardown does not
        delete them, so pinning them into the launch config is enough for the
        relaunched process to pick them back up.
        """
        config = getattr(self, "_launch_config", None)
        base = getattr(self, "_resolved_xdg_base", None)
        if config is None or base is None:
            return False
        config["xdg_base"] = base
        config["credential_dir"] = self._resolved_credential_dir
        config["keyring_app"] = self._resolved_keyring_app
        # Record what we pinned so reset()'s `_clear_relaunch_pin` un-pins exactly
        # these, and never a value the caller supplied at launch.
        self._record_relaunch_pin(config, self._RELAUNCH_PIN_KEYS)
        return True

    def app_path(self) -> str | None:
        """The executable THIS launch ran, or None before the first launch.

        The artifact suite reads it back to assert on the thing that actually ran
        rather than on the path it believes was configured — the linux mirror of
        the macOS driver's `bundle_path()`, and for the same reason: an
        installed-product test that silently drove `target/debug/fauna-desktop`
        would be the most expensive kind of false pass.
        """
        config = getattr(self, "_launch_config", None)
        return None if config is None else config.get("app_path")

    def recover(self) -> bool:
        """Relaunch a wedged app so the next test gets a fresh instance.

        The common mid-suite failure is the app's GTK UI thread wedged (e.g. a
        reset()'s window close+rebuild never completes because a modal was left
        open). Kill it and launch a fresh process so one wedged test costs one
        test, not the rest of the session.
        """
        config = getattr(self, "_launch_config", None)
        if config is None:
            return False
        # The app it is about to close may already be dead of a crash — the `app`
        # fixture reaches here when a reset was never acknowledged. The relaunch
        # goes ahead either way; the death is recorded for the phase.
        self._read_exit_before_close("the relaunch in recover()")
        self._stop_app()
        try:
            self.launch(config)
            return True
        except Exception:
            return False

    def seed_pending_factory_reset(
        self, nest_url: str, handle: str, claim_code: str
    ) -> None:
        """CR-2 slot seam (linux File backend). The client-local store is the flat
        `{FAUNA_E2E_CREDENTIAL_DIR}/{FAUNA_KEYRING_APP}.json` this launch pinned
        (`_resolved_credential_dir` / `_resolved_keyring_app`); `CredentialStore`
        keys every registry logical key verbatim, so the active account's
        per-actor `fauna/<actor>/pending_factory_reset` record is what the
        reconcile reads back — the same shared-Rust path every app takes. See
        `PlatformDriver.seed_pending_factory_reset` for the contract.

        Requires `preserve_state_across_relaunch()` to have pinned this launch's
        store (the journey does that before calling here); without it `launch()`
        hands the relaunch a fresh dir and the seeded slot would not survive."""
        cred_dir = getattr(self, "_resolved_credential_dir", None)
        app = getattr(self, "_resolved_keyring_app", None)
        if not cred_dir or not app:
            raise RuntimeError(
                "seed_pending_factory_reset() called before a launch that resolved "
                "the file-backed credential store — pin it via "
                "preserve_state_across_relaunch() first"
            )
        self._ensure_pending_factory_reset_in_json_file(
            os.path.join(cred_dir, f"{app}.json"),
            nest_url,
            handle,
            claim_code,
            native_mirror_prefix=None,
        )

    # `set_provider_base_urls` and `call_machine_method` are both the shared
    # `HttpBridgeDriver` impls. Linux used to override the latter with its own
    # `{"action": "machine", "state": {name, json_arg}}` wire shape — the only
    # sender of that shape in the fleet. The base now carries linux's richness
    # (it returns `state.machine_method_result`) on the shape the other four
    # native apps already speak, so the override is gone rather than copied
    # onto a seventh client.
