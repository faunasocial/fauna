"""CLI (fauna-tui TUI) driver — in-process automation agent over HTTP.

The tui app hosts the same in-process agent as linux (the shared
`libs/fauna-e2e-agent` HTTP front-end on a per-instance FAUNA_E2E_AGENT_PORT;
see docs/goal/architecture/apps/tui.md § E2E automation), so this driver is
a thin HttpBridgeDriver subclass: launch + teardown only, every element/state
op inherited.

Launch runs the binary inside a **pty** — crossterm needs a real tty to enter
raw mode / the alternate screen, and a pty keeps the tier_3 path honest (the
real terminal backend, not a test backend). The pty allocation is the **one**
platform-divergent seam — POSIX `pty`/`fcntl`/`termios`, Windows ConPTY
(`pywinpty`) — factored behind `pty_backend.spawn_pty`; everything below (the
drain loop, the DA1 answer, frame capture) speaks only the `PtyBackend` contract
and is identical on both. The pty master MUST be drained: ratatui writes every
frame to it, and an undrained pty buffer (~64K) fills and blocks the app's
render loop mid-`draw`. A daemon thread streams master → `app.out` (also giving
post-mortem frame dumps); on POSIX stderr goes to a separate file so panics stay
readable instead of interleaving with frames (ConPTY attaches the whole console,
so on Windows stderr rides the stream into `app.out`).

**The driver is the terminal emulator.** Owning the pty master is exactly that
role, and two of its duties are load-bearing for `tui.md` § Rendering's inline
images, which the client auto-detects and never configures:

- it reports a **cell size** (`_PTY_CELL_W`/`_PTY_CELL_H` on the winsize ioctl),
  which is how the client sizes an image to its cell box; and
- it **answers the client's `ESC [ c` probe** (`_DA1_SIXEL` / `_DA1_NO_SIXEL`),
  which is how the client learns whether this terminal does sixel.

So a graphics test says `tui_da1_sixel: True` in its launch config and the
client's real probe → real decision → real emit runs end to end. There is no
app-side override to force a protocol, and there must not be one: "auto-detected,
never configured" is the goal doc's wording, and a knob would be the thing it
forbids. The driver changes what the *terminal* says, never what the client does
with the answer.

**The DA1 duty is complete on both backends; the cell-size duty is POSIX-only.**
ConPTY relays the client's `ESC [ c` on the master and forwards the reply to the
app's stdin, so since the client's Windows probe arm landed (2026-08-01,
`graphics::detect::probe_da1`) detection runs for real on Windows too — the
inline-image suite exercises the real probe → decide → emit path on both. What
ConPTY has no concept of is *pixel* size, so the Windows backend cannot report a
cell size and the app falls back to its own assumed one: a smaller picture in the
same cell box, never a missing one. A test that asserts on image geometry
therefore reads :meth:`TuiDriver.reports_pixel_size` — a terminal capability, not
an OS check (`testing.md` point 7).
"""

import base64
import json
import os
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

from .base import detached_agent_log_text, rolling_log_text, stitch_agent_log
from .http_bridge import HttpBridgeDriver
from .port_util import (
    find_free_port,
    reap_descendants_of,
    terminate_tree,
    track_process,
    untrack_process,
)
from .pty_backend import spawn_pty

# A real-ish window: the sidebar (24 cols) plus a page pane.
_PTY_ROWS = 40
_PTY_COLS = 120

# The pty's cell size in pixels. A real terminal reports its window's pixel size
# alongside its size in cells, and `fauna-tui` divides the two to size an inline
# image (`apps/fauna-tui/src/graphics`). Reporting zeroes — the default, and what
# this pty said before — makes the app fall back to an assumed cell size, so the
# harness would silently never exercise the real path. These are ordinary cell
# dimensions.
_PTY_CELL_W = 8
_PTY_CELL_H = 16

# Primary-device-attributes replies. A terminal answers `ESC [ c` with what it
# is; attribute **4** is sixel graphics, and it is the only attribute the client
# acts on (`graphics::detect`).
#
# The pty has no terminal emulator behind it, so nothing would answer at all and
# every launch would sit out the client's probe timeout before falling back. The
# driver owns the pty master, which is exactly the role a terminal emulator
# plays — so it answers, and a test picks which terminal it is answering as.
# Nothing about the client changes: it probes for real and decides for real.
_DA1_SIXEL = b"\x1b[?64;1;2;4;6;9;15c"
_DA1_NO_SIXEL = b"\x1b[?64;1;2;6;9;15c"

# The query the client sends.
_DA1_QUERY = b"\x1b[c"

# The clipboard write a terminal honours: OSC 52, selection `c`, base64 payload,
# terminated by BEL or ST. The client copies this way (`wizard::copy_to_clipboard`)
# and the driver owns the pty master — the terminal-emulator role again — so the
# stream is where a copy is observable, the same way a real terminal sees it.
_OSC52_COPY = re.compile(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)")

# macOS's `sun_path` cap, and the fixed suffix the sync agent's socket adds to
# the launch HOME (`fauna_ipc::unix_transport::default_socket_path`, the
# `cfg(target_os = "macos")` arm). Mirrors `drivers/macos.py`'s constants of the
# same names — same kernel limit, same derivation, so the two drivers state it
# once each rather than one importing the other's private.
_SUN_PATH_MAX = 104
_AGENT_SOCKET_RELPATH = "Library/Application Support/Fauna/sync-agent.sock"


def _relocates_home(sys_platform: str | None = None) -> bool:
    """Whether a launch on `sys_platform` moves ``HOME`` into its own tmpdir.

    darwin only, and stated once: `build_launch_env` does the move, and
    `TuiDriver._remember_launch_store` records the result for
    `preserve_state_across_relaunch()` — two spellings of the rule are how the
    pin would come to cover a HOME the launch never relocated (linux)."""
    return (sys_platform or sys.platform) == "darwin"


def _account_store_root(env: dict, sys_platform: str | None = None) -> str:
    """The unified account-store root a launch with `env` resolves — the seat's
    own spelling of `fauna_account_store::root::production_base()`: windows
    `%LOCALAPPDATA%\\Fauna\\sync`, darwin `<HOME>/Library/Application Support/
    Fauna/sync` (the relocated HOME `build_launch_env` sets), linux
    `<XDG_CONFIG_HOME>/fauna/sync`."""
    platform = sys_platform or sys.platform
    if platform == "win32":
        return os.path.join(env["LOCALAPPDATA"], "Fauna", "sync")
    if platform == "darwin":
        return os.path.join(env["HOME"], "Library", "Application Support", "Fauna", "sync")
    return os.path.join(env["XDG_CONFIG_HOME"], "fauna", "sync")


def launch_tmp_root(sys_platform: str) -> str | None:
    """Where a launch's tmpdir is created, or None for the platform default.

    macOS only: its default ``TMPDIR`` is ``/var/folders/<28 opaque chars>/T/``
    (~48 bytes), which plus a relocated HOME and the 50-byte socket suffix
    overruns ``sun_path`` — the agent then spawns and dies at bind while the app
    reports only ``agent unreachable``. ``/tmp`` keeps the derived socket ~90
    bytes. The same move ``drivers/macos.py`` made.

    Elsewhere the default stands: linux's socket is ``$XDG_RUNTIME_DIR``-derived
    (short, and isolated above) and windows has no ``/tmp`` at all.
    """
    return "/tmp" if sys_platform == "darwin" else None


def build_launch_env(
    config: dict, port: int, tmp: str, sys_platform: str | None = None
) -> dict:
    """The child environment for one isolated fauna-tui launch.

    Mirrors the linux driver's per-app isolation: fresh XDG dirs + a unique
    keyring namespace + a file-backed credential dir (headless boxes have no
    usable Secret Service), so the app boots clean and concurrent runs don't
    collide. Split out pure for the tier_1 unit test.

    **Persistent-credential mode.** The defaults derive the keyring namespace
    from the (per-launch) agent port and the credential dir from the (per-launch)
    tmpdir, so nothing survives a force-quit + relaunch. Launch-routing tests
    need the opposite: the identity trio must be read back on the next launch.
    Passing ``keyring_app`` / ``credential_dir`` / ``xdg_base`` pins each to a
    stable value the caller controls, which is tui's analogue of the linux
    driver's ``use_real_keyring`` mode — except it rides the shared store's
    ``FAUNA_E2E_CREDENTIAL_DIR`` file backend (``libs/fauna-credential-store``)
    rather than the session Secret Service, so it needs no unlocked keyring and
    never touches the user's real credentials.
    """
    env = dict(os.environ)
    env.update(config.get("environment", {}))
    # A `RUST_LOG` the human exported survives a launch config that sets its own:
    # the `update` above would otherwise replace it wholesale, so a diagnosis run
    # asking for `fauna_anon_client=debug` silently got only the harness's
    # targets. Appended, so both sets of directives apply (the windows driver's
    # inheritance of the same variable, `drivers/windows.py`).
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

    # ── macOS: relocate HOME, or the agent socket is the machine-global one ───
    #
    # `fauna_ipc::unix_transport::default_socket_path` branches on the platform:
    # the linux arm reads `$XDG_RUNTIME_DIR` (isolated below), but the macOS arm
    # derives the socket from `dirs::home_dir()` and consults NO XDG variable. So
    # on darwin the XDG isolation below buys nothing for the agent — a launch
    # inheriting the real HOME resolves `~/Library/Application Support/Fauna/
    # sync-agent.sock`, which on a dev box is bound by the INSTALLED
    # `/Applications/Fauna.app` agent. The tui app would then drive the
    # developer's real sync agent: e2e folder binds and content-key pushes
    # landing in the real install, against the live nest.
    #
    # That is testing.md point 10's machine-global leak class, and the same shape
    # that hid the broken windows and macOS seats behind a shared agent
    # (2026-07-24) — a seat that looks bound while syncing someone else's state.
    # `CFFIXED_USER_HOME` rides along for the same reason `drivers/macos.py` sets
    # it: CoreFoundation's home notion honors it and not `HOME`.
    #
    # darwin-only by intent: linux's seat is the one with a long green record and
    # its socket is already isolated, so moving HOME there would be an unverified
    # change to the only working seat. Gate: `test_tui_driver_env.py`.
    if _relocates_home(sys_platform):
        home = config.get("home") or os.path.join(tmp, "home")
        # The bindability check is gated on the real-agent marker, exactly as
        # `drivers/macos.py`'s twin is: relocating HOME is unconditional (even an
        # agent-less launch polls `GetServiceStatus` and must not reach the real
        # socket), but the many launches that never spawn an agent are
        # legitimately free to sit at a long path — pytest's own `tmp_path` is
        # ~90 bytes before the suffix.
        sock = os.path.join(home, _AGENT_SOCKET_RELPATH)
        wants_agent = config.get("environment", {}).get("FAUNA_E2E_REAL_SYNC_AGENT")
        if wants_agent and len(sock.encode()) >= _SUN_PATH_MAX:
            raise RuntimeError(
                f"launch HOME {home!r} derives a {len(sock.encode())}-byte "
                f"sync-agent socket path, over macOS's sun_path limit of "
                f"{_SUN_PATH_MAX - 1} usable bytes:\n  {sock}\nThe agent would "
                f"spawn and die at bind ('path must be shorter than SUN_LEN'), "
                f"and the app would report only 'agent unreachable'. Pin a "
                f"shorter config['home'] (under {launch_tmp_root('darwin')})."
            )
        os.makedirs(home, exist_ok=True)
        env["HOME"] = home
        env["CFFIXED_USER_HOME"] = home

    xdg_base = config.get("xdg_base") or tmp
    xdg_data = os.path.join(xdg_base, "data")
    xdg_config = os.path.join(xdg_base, "config")
    creds_dir = config.get("credential_dir") or os.path.join(tmp, "creds")
    for d in (xdg_data, xdg_config, creds_dir):
        os.makedirs(d, exist_ok=True)
    env["XDG_DATA_HOME"] = xdg_data
    env["XDG_CONFIG_HOME"] = xdg_config
    env["FAUNA_KEYRING_APP"] = config.get("keyring_app") or f"fauna-e2e-agent-{port}"
    if config.get("headless_store"):
        # Drive the SEALED passphrase backend (apps/tui.md § Credential
        # storage): an empty FAUNA_E2E_CREDENTIAL_DIR reads as unset (the store
        # filters empties), and the force var is the test-only carve-out that
        # skips the keyring probe — a desktop dev box would otherwise resolve
        # to its real Secret Service. The sealed file lands under the pinned
        # XDG_CONFIG_HOME, so `preserve_state_across_relaunch()` covers it.
        env["FAUNA_E2E_CREDENTIAL_DIR"] = ""
        env["FAUNA_E2E_FORCE_HEADLESS_STORE"] = "1"
    else:
        env["FAUNA_E2E_CREDENTIAL_DIR"] = creds_dir
    env["FAUNA_E2E_AGENT_PORT"] = str(port)

    # E2e download-dir seam (mirrors drivers/linux.py): the app's
    # `backups.rs::download_dir()` prefers FAUNA_E2E_DOWNLOAD_DIR, so pointing
    # it into the per-launch scratch dir makes dialog-less saves (backups
    # single-file download, Export My Data) observable via `download_dir()`
    # without ever touching the box's real ~/Downloads. A caller-supplied
    # override in `config["environment"]` (merged above) wins.
    env.setdefault("FAUNA_E2E_DOWNLOAD_DIR", os.path.join(tmp, "e2e-downloads"))
    os.makedirs(env["FAUNA_E2E_DOWNLOAD_DIR"], exist_ok=True)

    # Private XDG_RUNTIME_DIR (0700, per the basedir spec) — the tui twin of the
    # linux driver's isolation (drivers/linux.py). The external `fauna-sync-agent`
    # tui direct-spawns under e2e (`sync_agent.rs`, A6) puts its control socket at
    # `$XDG_RUNTIME_DIR/fauna/sync-agent.sock`, so inheriting the REAL runtime dir
    # would route every e2e launch — and the developer's own desktop session — to
    # ONE shared agent (testing.md § point 10's machine-global leak class; same
    # shape as the linux D-Bus/runtime-dir fix). Because the app direct-spawns the
    # agent as a child, both ends inherit this private dir and the socket is
    # per-launch by construction.
    #
    # **A caller-passed `XDG_RUNTIME_DIR` OWNS it** — the carve-out shape
    # `drivers/linux.py` already uses for `DBUS_SESSION_BUS_ADDRESS`
    # (testing.md § point 10). This dir is not only the agent socket's home: it
    # is also the external-media handoff base on Linux (`apps/tui.md`
    # § External media handoff → Location), so a test that must *assert on the
    # handed-off path* has to name the dir itself. Assigning the private default
    # unconditionally silently overrode `config["environment"]` — which is merged
    # ABOVE, before this line — and made every such assertion compare against a
    # dir the test never chose. Both `test_tui_media_external_open.py` and
    # `test_folder_member_media_decrypt.py` pin exactly that path.
    #
    # The isolation property survives because a caller passing this var passes a
    # **private** dir (a `tmp_path` subdir) — both callers do. Passing the box's
    # REAL runtime dir would re-open the machine-global agent-socket leak this
    # default exists to close; don't.
    xdg_runtime = config.get("environment", {}).get("XDG_RUNTIME_DIR") or os.path.join(
        xdg_base, "runtime"
    )
    os.makedirs(xdg_runtime, mode=0o700, exist_ok=True)
    env["XDG_RUNTIME_DIR"] = xdg_runtime

    # Windows has no path-derived agent seam to isolate: the agent rendezvouses
    # on the machine-global `\\.\pipe\fauna-sync.<SID>` and defaults its state to
    # `%LOCALAPPDATA%`, so a private XDG world buys nothing for the AGENT and all
    # three isolations must be named explicitly (`sync-agent.md` § Per-launch pipe
    # isolation; the same trio `_apply_isolated_sync_agent_env` passes the
    # windows app). Without them a `--app tui` run drives — and corrupts —
    # the developer's own installed agent (e2e-conventions.md § point 10). The
    # relocation below closes the same variable for the APP process, which is a
    # different leak at the same seam; the explicit agent pins stay, because
    # `--data-dir` also drops the `fauna/sync` suffix (the dir itself is the
    # base; the per-actor subdir nests under it as on every layout) and
    # `sync_agent_state_base` reads that base back.
    if (sys_platform or sys.platform) == "win32":
        # `%LOCALAPPDATA%` — the fourth thing a private XDG world buys nothing
        # for, and the only DESTRUCTIVE one. tui's flat base is XDG-derived on
        # every platform (`session::config_dir` → `xdg_app_config_dir`), so the
        # relocation above isolates it here too — but the unified account
        # store is NOT under that base: `account_scope::erase` passes
        # `StoreRoot::platform().base()` as a second root, and shared Rust's
        # windows arm (`fauna_account_store::root::production_base`) derives it
        # from `LOCALAPPDATA`, ignoring XDG entirely. So a `--app tui` sign-out
        # on windows ran `remove_dir_all` over every 64-hex actor dir in the
        # developer's real `%LOCALAPPDATA%\Fauna\sync` — the tui twin of the
        # windows app's own leak, found while
        # fixing that one. Derived from `xdg_base`, not `tmp`, so a caller
        # pinning `xdg_base` for `preserve_state_across_relaunch()` preserves
        # this root across the relaunch the same way it preserves the flat base.
        # `env` starts as a copy of `os.environ`, so this must ASSIGN rather
        # than `setdefault` — the real value is always already present.
        local_appdata = config.get("environment", {}).get("LOCALAPPDATA") or os.path.join(
            xdg_base, "localappdata"
        )
        os.makedirs(local_appdata, exist_ok=True)
        env["LOCALAPPDATA"] = local_appdata
        # Per-launch pipe leaf, keyed on the agent port because that is already
        # this launch's unique id (`FAUNA_KEYRING_APP` uses it too) — a pid would
        # be the harness's, shared by every launch in the run.
        env.setdefault("FAUNA_E2E_SYNC_PIPE", f"fauna-sync-e2e-tui-{port}")
        env.setdefault(
            "FAUNA_E2E_SYNC_AGENT_DATA_DIR", os.path.join(tmp, "sync-agent")
        )
        # PIN the binary, never leave it to the sibling probe: an unpinned miss
        # falls through to whatever `fauna-sync-agent.exe` the box has installed,
        # which reads as "bound but syncing nothing" instead of failing (the
        # 2026-07-24 multiseat lesson, `agent_spawner::AGENT_BIN_ENV`). The
        # sibling of the app binary IS the right target and needs no second
        # builder: `just tui-debug` builds `fauna-tui` and `fauna-sync-agent`
        # together, exactly as `linux-debug` does.
        app_path = config.get("app_path")
        if app_path and "FAUNA_E2E_SYNC_AGENT_BIN" not in env:
            env["FAUNA_E2E_SYNC_AGENT_BIN"] = os.path.join(
                os.path.dirname(os.path.abspath(app_path)), "fauna-sync-agent.exe"
            )

    # The TUI probes the terminal; pin a capable, ubiquitous terminfo entry so
    # rendering doesn't depend on the harness host's TERM.
    env["TERM"] = "xterm-256color"
    return env


class TuiDriver(HttpBridgeDriver):
    """Drives fauna-tui via its in-process automation agent."""

    # tui's OS region setting is the POSIX locale's territory.
    REGION_SOURCE_KEY = "source_system_locale"

    def log_scope_across_relaunch(self) -> str:
        """``"per-launch"`` — each `launch()` mints a fresh tmp dir and reopens `app.err` inside it.

        See the base declaration for what each answer means and why it is
        declared rather than inferred; pinned per driver by
        `tests/test_module_relaunch.py`.
        """
        return "per-launch"

    def __init__(self):
        super().__init__(bridge_url="")
        self._pty = None
        self._drain_thread = None
        self._app_stderr = None

    def is_tui(self) -> bool:
        return True

    def download_dir(self) -> str | None:
        """Where fauna-tui saves e2e-mode downloads (no save dialog on a
        terminal — `backups.rs::download_dir()` prefers `FAUNA_E2E_DOWNLOAD_DIR`,
        which `build_launch_env` points into the per-launch scratch dir).
        Mirrors `drivers/linux.py::download_dir`; None before `launch()`."""
        return getattr(self, "_download_dir", None)

    @property
    def config_home(self) -> str:
        """The per-launch `XDG_CONFIG_HOME` this instance was isolated into.

        tui's analogue of the linux driver's `config_home`: device-local state
        rests under `<config_home>/fauna` — notably the direct-spawned
        `fauna-sync-agent`'s own source of truth at
        `<config_home>/fauna/sync/config.toml` (the folder↔set bindings it
        persists, `sync-agent.md` § Control plane split). A test asserting an
        on-disk side effect of a Folders binding reads it from here. Raises if
        accessed before `launch()`.
        """
        cfg = getattr(self, "_xdg_config", None)
        if cfg is None:
            raise RuntimeError("config_home accessed before launch()")
        return cfg

    @property
    def sync_agent_state_base(self) -> str | None:
        """Where THIS launch's agent keeps its state, when that is not derivable
        from `config_home`.

        `None` on unix, where the agent inherits the isolated XDG world and
        `<config_home>/fauna/sync` is the answer. On windows there is no such
        derivation: the agent's default root is the machine-global
        `%LOCALAPPDATA%`, so the driver passes `--data-dir` explicitly (via
        `FAUNA_E2E_SYNC_AGENT_DATA_DIR`) and that dir — no `fauna/sync` suffix,
        since `--data-dir` suppresses the derivation; the per-actor subdir nests
        under it as on every layout — is the base.
        """
        if os.name != "nt":
            return None
        return getattr(self, "_sync_agent_data_dir", None)

    @property
    def sync_agent_pipe(self) -> str | None:
        """The full Win32 pipe path THIS launch's agent serves, for a test that
        must talk to it directly — `None` off windows (the agent listens on a
        unix socket there) or before launch. Same property on the windows
        driver."""
        if os.name != "nt":
            return None
        leaf = getattr(self, "_sync_agent_pipe_leaf", None)
        return rf"\\.\pipe\{leaf}" if leaf else None

    # The tui agent serves POST /element/scroll-into-view as a REAL scroll: its
    # viewport follows the focus ring, so the agent lands the ring on the target
    # (or the first focusable element after it) exactly as the arrow keys would,
    # and the next frame paints it in view. It errors rather than acking when
    # nothing moved — found/error semantics identical to linux's.
    _supports_scroll_into_view = True

    def in_viewport(self, element_id: str, *, index: int = 0,
                    scope: str | None = None) -> bool:
        """The agent's ``in-viewport`` attribute: true when the element's middle
        line lies inside the band the last frame painted — linux's rule (a
        widget's vertical centre inside its scroller's visible band), in terminal
        lines. tui's registry lists every element whether or not it painted, so,
        as on linux, the registry alone cannot say "on screen"; the frame's own
        geometry does (``crate::ui::PaintedPage``)."""
        value = self.get_attr(element_id, "in-viewport", index, scope=scope)
        if value is None:
            raise AssertionError(
                f"tui answered no in-viewport for {element_id!r}[{index}] — the "
                "element is missing or painted no line this frame; "
                f"{self.diagnose(element_id, scope=scope)}"
            )
        return value == "true"

    # ------------------------------------------------------------------
    # Process lifecycle
    # ------------------------------------------------------------------
    def frame_stream_path(self) -> str:
        """Everything the client has written to its terminal, verbatim.

        The pty stream, escapes and all — which for most purposes is a
        post-mortem dump, but is the only place inline-image output is
        *observable*: a graphics protocol paints past ratatui straight to stdout,
        so it reaches no frame buffer and no element (`tui.md` § Rendering).

        Raises if the driver has not launched.
        """
        if self._tmp_dir is None:
            raise RuntimeError("frame_stream_path() before launch()")
        return os.path.join(self._tmp_dir, "app.out")

    def get_clipboard_text(self) -> str | None:
        """The text of the app's most recent clipboard copy, or ``None``.

        A terminal app has no clipboard of its own: it asks the terminal to set
        one by writing OSC 52 to its stdout, and the terminal decodes it. The
        driver IS that terminal (module docstring), so the copy is read where a
        real one would read it — the pty stream — rather than faked through an
        automation hook. ``None`` until the app has copied anything this launch.

        Raises if the driver has not launched.
        """
        with open(self.frame_stream_path(), "rb") as stream:
            copies = _OSC52_COPY.findall(stream.read())
        if not copies:
            return None
        return base64.b64decode(copies[-1]).decode("utf-8")

    def reports_pixel_size(self) -> bool:
        """Whether this launch's terminal told the app its cell size in pixels.

        POSIX ptys do (the winsize ioctl carries pixel dimensions); ConPTY has no
        pixel-size concept, so the app falls back to its own assumed cell size
        there. A test asserting on an inline image's *geometry* branches on this
        rather than on the OS — it is the terminal's capability that decides the
        answer, and the client's behaviour is identical either way (module
        docstring; `testing.md` point 7).

        Raises if the driver has not launched.
        """
        if self._pty is None:
            raise RuntimeError("reports_pixel_size() before launch()")
        return self._pty.reports_pixel_size

    def is_app_alive(self) -> bool:
        """True while the launched ``fauna-tui`` process is still running.

        tui's analogue of the linux driver's method of the same name, and the
        same contract — the pty backend wraps a ``Popen`` and answers
        ``poll()``. Its job is the *positive liveness* half of an
        absence-shaped assertion: a test that proves "the second instance must
        NOT survive" is vacuously green if the first instance had quietly died,
        so the served instance is asserted live first
        (``test_account_instance_lock_linux.py`` establishes the pattern).
        """
        proc = getattr(self, "_pty", None)
        return proc is not None and proc.poll() is None

    def wait_app_exit(self, timeout: float = 10.0) -> bool:
        """Block until the launched process exits; True if it exited within
        ``timeout``. tui's analogue of the linux/windows drivers' method of the
        same name and contract — the pty backend wraps a ``Popen``-compatible
        object, so this is the same ``wait(timeout=...)`` shape. The leave-door
        for tui is the real ``exit-tab`` click (`automation.rs`'s
        `click_exit_tab_quits_via_the_agent_path` — the same door
        `should_quit` opens for a real user), not a signal: a test drives it
        with an ordinary ``click("exit-tab")`` and then waits here for the
        process to actually end.
        """
        proc = getattr(self, "_pty", None)
        if proc is None:
            return True
        try:
            proc.wait(timeout=timeout)
            return True
        except subprocess.TimeoutExpired:
            return False

    def app_stderr_text(self) -> str:
        """This launch's captured app log so far ("" if none yet).

        tui's analogue of the linux driver's ``app_stderr_text()`` (same
        contract, so a client-parametrized launch-flow test reads both
        uniformly): the log carries decisions that reach no frame buffer and
        no element — the deployment-seed custody leg's "custodied off-box on
        the account plane" outcome line, say — so a *positive* wait on them (not a blind sleep) is the only race-free way
        to know a fire-and-forget background task finished before teardown.

        On POSIX stderr is captured to its own ``app.err`` file (kept off the
        frame stream so a panic stays readable, not shredded by escapes); on
        Windows stderr is the ConPTY terminal, so tui turns its stderr layer
        off and ``app.err`` stays empty — there the answer is tui's own
        rolling file under ``<XDG_CONFIG_HOME>/fauna-tui/logs/``, and only
        when that is absent too the pty stream (``app.out`` /
        :meth:`frame_stream_path`), which is frames rather than log lines.
        """
        tmp = getattr(self, "_tmp_dir", None)
        if not tmp:
            return ""
        try:
            err = Path(os.path.join(tmp, "app.err")).read_text(errors="replace")
        except OSError:
            err = ""
        if not err.strip():
            # stderr is the ConPTY terminal on windows, so tui's stderr layer is
            # off (`session::install_logging`) — but its rolling file is not.
            # Those are the app's own words; the pty stream below is only frames.
            xdg = getattr(self, "_xdg_config", None)
            err = rolling_log_text(os.path.join(xdg, "fauna-tui") if xdg else None)
        if not err.strip():
            try:
                err = Path(os.path.join(tmp, "app.out")).read_text(errors="replace")
            except OSError:
                pass
        # On WINDOWS the agent is spawned DETACHED (the OS's constraint, not the
        # app's — `sync_agent.rs::platform_spawner`'s #[cfg(windows)] arm), so
        # none of its output reaches `app.err` or the pty stream above. Without
        # this, `await_agent_upload` on a tui seat here waits on a stream the
        # evidence can never arrive in: the upload succeeds and the witness
        # still fails. On POSIX the agent is an inherited-fd child and its
        # output is already in `err`, so `sync_agent_state_base` is None and
        # this contributes nothing.
        return stitch_agent_log(err, detached_agent_log_text(self.sync_agent_state_base))

    def inherited_agent_log_text(self) -> str:
        """On POSIX the agent is an inherited-fd child (its lines are already in
        `app_stderr_text()`); its own rolling log under `<config_home>/fauna/sync`
        tells them apart. Windows stitches the detached agent instead."""
        if self.sync_agent_state_base is not None:
            return ""
        cfg = getattr(self, "_xdg_config", None)
        return rolling_log_text(os.path.join(cfg, "fauna", "sync")) if cfg else ""

    def _remember_launch_store(
        self, config: dict, env: dict, tmp: str, sys_platform: str | None = None
    ) -> None:
        """Remember where this launch's client-local store landed, so
        `preserve_state_across_relaunch()` can pin it if the test asks. Not
        pinned by default: the harness contract is that a relaunched native app
        comes back with a FRESH store (see `HttpBridgeDriver.hard_reload`). The
        `xdg_base` default matches `build_launch_env`'s (`config["xdg_base"] or
        tmp`); the credential dir + keyring app come straight back from `env`.

        `home` is the one that differs by platform: only darwin relocates HOME
        (`_relocates_home`), and only there does the nest-identity pin store —
        `<HOME>/Library/Application Support/Fauna/trust`, shared Rust's
        `install_scoped_trust_home` — rest under it instead of under the pinned
        `XDG_CONFIG_HOME`. Elsewhere it is None: the launch inherited the box's
        own HOME, so there is nothing of the launch's to pin."""
        self._resolved_xdg_base = config.get("xdg_base") or tmp
        self._resolved_credential_dir = env["FAUNA_E2E_CREDENTIAL_DIR"]
        self._resolved_keyring_app = env["FAUNA_KEYRING_APP"]
        self._resolved_home = env["HOME"] if _relocates_home(sys_platform) else None

    def launch(self, config: dict) -> None:
        self._launch_config = config
        port = find_free_port()
        self._agent_port = port
        self._url = f"http://127.0.0.1:{port}"

        tmp = tempfile.mkdtemp(
            prefix="fauna-e2e-tui-agent-", dir=launch_tmp_root(sys.platform)
        )
        self._tmp_dir = tmp
        env = build_launch_env(config, port, tmp)
        # The store principal survives a relaunch (convention 10's carve-out,
        # `HttpBridgeDriver._begin_principal_slot_carry`): harvest the launch being
        # replaced BEFORE the `_resolved_*` store attrs below move to this one.
        self._begin_principal_slot_carry(config, env)
        # The per-launch XDG_CONFIG_HOME this instance was isolated into — where
        # the direct-spawned `fauna-sync-agent` writes its `fauna/sync/config.toml`
        # (a test asserting an on-disk agent side effect reads it via `config_home`).
        self._xdg_config = env["XDG_CONFIG_HOME"]
        # Windows only (empty elsewhere): the explicit agent state root this
        # launch pinned, which `sync_agent_state_base` hands to the config
        # reader — see that property.
        self._sync_agent_data_dir = env.get("FAUNA_E2E_SYNC_AGENT_DATA_DIR")
        # Windows only: the pipe leaf this launch's agent serves — see
        # `sync_agent_pipe`.
        self._sync_agent_pipe_leaf = env.get("FAUNA_E2E_SYNC_PIPE")
        # The unified account-store root THIS launch resolved, as shared Rust's
        # `production_base()` resolves it on each seat (`fauna_account_store::
        # root`): `LOCALAPPDATA` on windows, the relocated HOME's Application
        # Support on darwin, XDG elsewhere. Published for the same reason the
        # windows driver publishes its own — a harness-side re-spelling of the
        # derivation is exactly how the two drift apart — and after the carry
        # began, so the harvest above read the launch being replaced; the carry
        # restores the signing-in actor's replica under it beside the slot.
        self._resolved_store_root = _account_store_root(env)
        # …and the install device secret every account's sync device id is
        # derived from, laid down now, before the app starts (it names no
        # account). tui's install dir is `<XDG_CONFIG_HOME>/fauna-tui/sync` on
        # every platform (`account_scope.rs::install_sync_dir_under` over
        # `session::config_dir`, XDG-first) — not the account-store root above.
        self._carry_install_device_secret(os.path.join(self._xdg_config, "fauna-tui", "sync"))
        # Where dialog-less saves land (backups.rs::download_dir prefers this
        # var) — read back from the env build_launch_env produced, same shape
        # as drivers/linux.py's download_dir().
        self._download_dir = env.get("FAUNA_E2E_DOWNLOAD_DIR")

        self._remember_launch_store(config, env, tmp)

        # Seed the file-backed credential store BEFORE boot (mirrors
        # drivers/linux.py): the File backend maps every logical key straight
        # to the file key, so a test seeds the `AccountRegistry` state the app
        # reads (the `fauna/index` blob + per-actor slots) built with
        # `common.accounts.build_registry_seed` — the account-switcher tests'
        # path to a ≥2-account install. Skipped under `headless_store` (which zeroes
        # `FAUNA_E2E_CREDENTIAL_DIR` to drive the sealed backend — a seed there has
        # nowhere to land).
        seed = config.get("seed_credentials")
        if seed and self._resolved_credential_dir:
            cred_file = os.path.join(
                self._resolved_credential_dir, f"{self._resolved_keyring_app}.json"
            )
            with open(cred_file, "w") as f:
                json.dump(dict(seed), f)
            os.chmod(cred_file, 0o600)

        cmd = [config["app_path"], *shlex.split(config.get("app_args", "") or "")]
        # stderr to its own file so a panic isn't shredded by frame escapes
        # (POSIX only — the Windows ConPTY backend attaches the whole console and
        # ignores this handle, so there stderr rides the stream into `app.out`).
        self._app_stderr = open(os.path.join(tmp, "app.err"), "w")
        self._pty = spawn_pty(
            cmd,
            env,
            _PTY_ROWS,
            _PTY_COLS,
            _PTY_CELL_W,
            _PTY_CELL_H,
            stderr_file=self._app_stderr,
        )
        track_process(self._pty)
        # Windows: bind the launch and everything it spawns to a kill-on-close
        # job. fauna-tui spawns `fauna-sync-agent`, which is BUILT to outlive
        # the app that started it — correct in production, an orphan in a test.
        # `terminate_tree` only reaches the process it was handed, so without
        # this the agent survives the run, keeps answering the launch's pipe and
        # holds its own .exe against the next `just tui-debug` (observed
        # 2026-07-24, the first win run of the tui agent leg). POSIX needs no
        # equivalent: `popen_group_kwargs()` already makes the launch a group
        # leader and the agent inherits the group.
        reap_descendants_of(getattr(self._pty, "pid", 0))

        # Drain the pty so the app's render loop never blocks on a full
        # buffer; keep the frames for post-mortem.
        out_path = os.path.join(tmp, "app.out")

        # Which terminal this pty answers as. Default: one without sixel, so the
        # client resolves to its half-block arm and every existing suite paints
        # the `▀` art `painted_thumbnail_count` reads. A graphics test asks for
        # the sixel-capable answer instead.
        da1_reply = _DA1_SIXEL if config.get("tui_da1_sixel") else _DA1_NO_SIXEL

        backend = self._pty

        def drain():
            # Enough of the previous chunk to still match a query that straddles
            # a read boundary.
            carry = b""
            with open(out_path, "wb") as out:
                while True:
                    chunk = backend.read(65536)  # b"" only at EOF (both backends)
                    if not chunk:
                        return
                    out.write(chunk)
                    # Flush: `app.out` is an assertion surface for the graphics
                    # suite, not only a post-mortem dump, so a test polling it
                    # must see what the app has already written.
                    out.flush()
                    window = carry + chunk
                    if _DA1_QUERY in window:
                        try:
                            backend.write(da1_reply)
                        except Exception:
                            return
                    carry = window[-(len(_DA1_QUERY) - 1):]

        self._drain_thread = threading.Thread(target=drain, daemon=True)
        self._drain_thread.start()

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            rc = self._pty.poll()
            if rc is not None:
                raise RuntimeError(
                    f"fauna-tui exited early (rc={rc}). diagnostic tail: "
                    f"{self._exit_diagnostics(tmp, out_path)[-800:]}"
                )
            if self._agent_health_ok():
                self._bridge_dead = False
                return
            time.sleep(0.3)
        raise RuntimeError(f"in-process agent never became healthy on {self._url}")

    @staticmethod
    def _exit_diagnostics(tmp: str, out_path: str) -> str:
        """Best text for an early-exit message: the POSIX stderr file if it has
        content, else the pty stream (Windows merges stderr into `app.out`)."""
        try:
            err = Path(os.path.join(tmp, "app.err")).read_text(errors="replace")
        except OSError:
            err = ""
        if err.strip():
            return err
        try:
            return Path(out_path).read_text(errors="replace")
        except OSError:
            return err

    def _agent_health_ok(self) -> bool:
        try:
            with urllib.request.urlopen(f"{self._url}/health", timeout=1) as r:
                return r.status == 200
        except Exception:
            return False

    def supports_unclean_kill(self) -> bool:
        """True once a fauna-tui is launched.

        tui owns its app child as a **pty session leader** (``self._pty``), not the
        ``self._app_proc`` Popen the inherited check looks for — so without this
        override every `killable_app` test skipped on tui as a *structural
        impossibility it is not*. (The inherited docstring listed tui among the
        drivers that "set ``self._app_proc``"; tui never has. That stale claim is
        what let 11 `test_crash_recovery_journeys.py` tests read as covered-or-
        legitimately-absent for as long as the tui driver has existed.)
        """
        return self._pty is not None

    def kill_uncleanly(self) -> None:
        """SIGKILL our OWN pty child's process GROUP (`PlatformDriver.kill_uncleanly`).

        The pty child is spawned ``start_new_session=True`` (`posix_pty`), so it is
        a session **and** process-group leader — killing the group takes
        ``fauna-sync-agent`` with it. That matters twice: power-loss semantics (the
        agent must not outlive the crash it should have died in), and process safety
        (an orphaned agent holds its build artifact against the next compile and
        answers a pipe the next launch expects to serve itself — testing.md
        § point 10). Same group-kill the native drivers do, reached through the pty
        backend instead of a Popen.

        SIGKILL is uncatchable: no SIGTERM handler, no atexit, no terminal restore
        — the genuine crash. The dead backend stays in ``self._pty`` so the next
        ``teardown()`` (via ``hard_reload()`` → ``recover()``) closes the master fd
        and untracks it; teardown already tolerates an exited child.
        """
        backend = self._pty
        if backend is None:
            # The inherited NotImplementedError carries the contract text.
            super().kill_uncleanly()
            return
        pid = getattr(backend, "pid", 0)
        # ConPTY has no process groups (and `os.killpg` does not exist on Windows),
        # so there the backend's own force-stop — which is job-object backed via
        # `reap_descendants_of` — is the whole guarantee.
        if os.name != "nt" and pid:
            try:
                os.killpg(os.getpgid(pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError, OSError):
                backend.kill()
        else:
            backend.kill()
        try:
            backend.wait(timeout=10)
        except Exception:
            # Already reaped, or the backend cannot report — either way the app is
            # gone, which is all this primitive promises.
            pass

    def teardown(self) -> None:
        # `terminate_tree`, per PtyBackend's own contract (a backend is
        # Popen-compatible precisely so it can be handed to it): fauna-tui
        # spawns `fauna-sync-agent` as a group child, and that agent is built to
        # outlive the app that started it. What used to reap it here was not
        # this teardown but the **pty** — the kernel SIGHUPs the foreground
        # group when the session leader exits — so an agent that ignored SIGHUP
        # the way daemons routinely do would have orphaned exactly as the native
        # macOS seat's did. On Windows this is behaviour-identical to the
        # hand-rolled version it replaces (terminate → wait → kill), and the
        # job object from `reap_descendants_of` stays the real guarantee there.
        backend = self._pty
        if backend is not None:
            terminate_tree(backend)
            untrack_process(backend)
            backend.close()
            self._pty = None
        stderr = getattr(self, "_app_stderr", None)
        try:
            stderr and stderr.close()
        except Exception:
            pass
        self._app_stderr = None

    def seed_pending_factory_reset(
        self, nest_url: str, handle: str, claim_code: str
    ) -> None:
        """CR-2 slot seam (tui, File backend — identical to linux's).

        tui's client-local store is the same flat
        `{FAUNA_E2E_CREDENTIAL_DIR}/{FAUNA_KEYRING_APP}.json` the File backend
        writes, keyed by every registry logical key verbatim — exactly as
        `drivers/linux.py`. The launch reconcile then reads the active
        account's GENUINE per-actor `fauna/<actor>/pending_factory_reset`
        record back through the shared launch adapter, so this drives the same
        shared-Rust path macOS and linux already prove rather than a shim that
        would pass vacuously.

        This override is **obligatory, not optional**: the base contract
        (`PlatformDriver.seed_pending_factory_reset`) is that any driver
        supporting an unclean kill must implement it rather than let journey 6
        skip. tui gained `supports_unclean_kill()`, which made
        the obligation live; until then the missing seam was masked by the
        confirm-click's own agent timeout (`PageOp::outlives_click`), so both
        journeys failed before ever reaching this call.

        Requires `preserve_state_across_relaunch()` to have pinned this launch's
        store (the journey does that first); without it `launch()` hands the
        relaunch a fresh dir and the seeded slot would not survive."""
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

    def app_path(self) -> str | None:
        """The executable THIS launch ran, or None before the first launch.

        The artifact suite reads it back to assert on the thing that actually ran
        rather than on the path it believes was configured — the tui twin of the
        linux driver's `app_path()` and the macOS driver's `bundle_path()`, for
        the same reason: an installed-product test that silently drove
        `target/debug/fauna-tui` would be the most expensive kind of false pass
        (`tests/artifact/test_tui_installed_product.py`).
        """
        config = getattr(self, "_launch_config", None)
        return None if config is None else config.get("app_path")

    def preserve_state_across_relaunch(self) -> bool:
        """Pin this launch's client-local store so a later `teardown()` + relaunch
        reads back what this process wrote — tui's analogue of linux's override.

        Off by default (see `build_launch_env`): each launch normally gets a fresh
        `mkdtemp` XDG base + credential dir and a keyring namespace derived from the
        per-launch agent *port*, so client-side durable state is thrown away on
        relaunch — fine for journeys whose recovery lives nest-side, load-bearing
        for the mid-claim journey (it wants the relaunched app on create-identity).

        The nest-identity-pin journey needs the opposite: the `DiskPinStore` tui
        installs at startup (`session::install_disk_pin_store`, at shared Rust's
        `install_scoped_trust_home()`) must survive the relaunch so a seeded pin is
        read back. That dir is `$XDG_CONFIG_HOME/fauna` on linux but
        `<HOME>/Library/Application Support/Fauna/trust` on macOS
        (`security.md` § Pin custody across processes, rule 1), so the pin covers
        BOTH roots: `xdg_base` + the credential dir + the keyring app make
        `build_launch_env` reuse the same XDG dirs, and on macOS — where every
        launch relocates HOME into its own tmpdir — `home` is pinned too, or the
        relaunch reads an empty trust dir and never sees the seeded pin. Teardown
        never deletes any of them, so the relaunched process picks them straight
        back up.
        """
        config = getattr(self, "_launch_config", None)
        base = getattr(self, "_resolved_xdg_base", None)
        if config is None or base is None:
            return False
        config["xdg_base"] = base
        config["credential_dir"] = self._resolved_credential_dir
        config["keyring_app"] = self._resolved_keyring_app
        pinned = set(self._RELAUNCH_PIN_KEYS)
        home = getattr(self, "_resolved_home", None)
        if home:
            # A `home` the caller supplied at launch is theirs, not ours to
            # un-pin — unless a previous preserve() of ours wrote it (a relaunch
            # between two calls re-resolves `home` from that very pin).
            if "home" not in config or "home" in getattr(
                self, "_preserve_pinned_keys", ()
            ):
                pinned.add("home")
            config["home"] = home
        # Record what we pinned so reset()'s `_clear_relaunch_pin` un-pins exactly
        # these, and never a value the caller supplied at launch.
        self._record_relaunch_pin(config, pinned)
        return True

    def recover(self) -> bool:
        """Relaunch a wedged app so the next test gets a fresh instance."""
        config = getattr(self, "_launch_config", None)
        if config is None:
            return False
        try:
            self.teardown()
        except Exception:
            pass
        try:
            self.launch(config)
            return True
        except Exception:
            return False
