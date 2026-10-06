"""A private D-Bus session bus for harness-launched Linux apps.

A GTK client inherits `DBUS_SESSION_BUS_ADDRESS` from whoever launched it, so an
e2e app launched on a dev box talks to **that box's real desktop session bus**.
That makes machine-global, time-varying desktop state an input to the test: the
canonical bite is `org.kde.StatusNotifierWatcher`, which decides whether
close-to-tray has somewhere to hide (`tray::should_hide_to_tray`) and therefore
whether closing the window *quits* or *hides* — it comes and goes under a live
GNOME session, so `wait_app_exit()` passed or failed by the hour (diagnosed
2026-07-17). This is the session-bus analogue of
`drivers/linux.py::_headless_render_cmd_env`, which already isolates the app from
the box's ambient *display*, and of the bundled-Chromium singleton retirement
(`docs/goal/architecture/testing.md` § Cross-app e2e conventions, point 9):
the fix for a machine-global dependency is to remove it, not to serialize on it.

Each bus is a `dbus-daemon` on a private socket in a temp dir, owning nothing —
so by default a client sees **no** tray host, **no** notification daemon, and no
secrets service, and every launch gets the same answer on every box.

**The no-activation config is load-bearing, not a detail.** The config carries no
`<servicedir>`, so the bus performs **no service activation**. A stock
`dbus-daemon --session` would auto-activate `org.freedesktop.secrets` on the
client's startup libsecret call, and a freshly-activated *locked* gnome-keyring
blocks on an unlock prompt that never comes — the client then hangs *before* its
automation agent starts, surfacing as the misleading "in-process agent never
became healthy". With no activation those lookups fast-fail `ServiceUnknown`,
which the client already handles, so startup proceeds (independently measured:
a private-bus relaunch is green, with credentials and without).

Subclass to own names on the bus — see `helpers/tray_bus.py`'s `TrayBus`, which
adds a fake `StatusNotifierWatcher` so the tray tests can drive host-present,
host-absent, and host-disappears-at-runtime deterministically.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time

from .port_util import (
    popen_group_kwargs,
    terminate_tree,
    track_process,
    untrack_process,
)

_BUS_CONF = (
    '<!DOCTYPE busconfig PUBLIC '
    '"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" '
    '"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">\n'
    "<busconfig>\n"
    "  <type>session</type>\n"
    "  <listen>{address}</listen>\n"
    # No <servicedir>: no service activation (see the module docstring).
    '  <policy context="default">\n'
    '    <allow send_destination="*" eavesdrop="true"/>\n'
    '    <allow eavesdrop="true"/>\n'
    '    <allow own="*"/>\n'
    "  </policy>\n"
    "</busconfig>\n"
)


class PrivateSessionBus:
    """A `dbus-daemon` on a private socket, performing no service activation."""

    def __init__(self):
        self._tmp = None
        self._daemon = None
        self.address: str | None = None

    def start(self) -> "PrivateSessionBus":
        """Start the daemon and set `self.address`. Idempotent."""
        if self.address is not None:
            return self
        if shutil.which("dbus-daemon") is None:
            # Loud, not a silent fall back to the ambient bus: that would restore
            # exactly the machine-dependent nondeterminism this class removes,
            # and it would do it invisibly.
            raise RuntimeError(
                "dbus-daemon not found — the linux e2e driver needs it to give each "
                "app launch a private session bus (install the 'dbus-daemon'/'dbus' "
                "package for your distribution)"
            )
        self._tmp = tempfile.mkdtemp(prefix="fauna-e2e-session-bus-")
        sock = os.path.join(self._tmp, "bus")
        self.address = f"unix:path={sock}"
        conf = os.path.join(self._tmp, "bus.conf")
        with open(conf, "w") as f:
            f.write(_BUS_CONF.format(address=self.address))
        # Group-leader + die-with-parent, like every other harness child: the bus
        # must not outlive a `kill -9`'d pytest (testing.md § point 9).
        self._daemon = subprocess.Popen(
            ["dbus-daemon", f"--config-file={conf}", "--nofork"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            **popen_group_kwargs(),
        )
        track_process(self._daemon)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if os.path.exists(sock):
                return self
            if self._daemon.poll() is not None:
                raise RuntimeError(
                    f"private dbus-daemon exited early (rc={self._daemon.returncode})"
                )
            time.sleep(0.02)
        raise RuntimeError("private dbus-daemon never created its socket")

    def stop(self) -> None:
        """Terminate the daemon and remove its temp dir. Idempotent."""
        d = self._daemon
        self._daemon = None
        if d is not None:
            terminate_tree(d)
            untrack_process(d)
        if self._tmp and os.path.isdir(self._tmp):
            shutil.rmtree(self._tmp, ignore_errors=True)
        self._tmp = None
        self.address = None
