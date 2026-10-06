"""A private session bus + a fake StatusNotifierWatcher, for the Linux
close-to-tray (Track 4) e2e tests.

The Linux app decides whether close-to-tray has somewhere to restore the
window from by whether a `org.kde.StatusNotifierWatcher` is present on its
**session bus** (vendored `libs/ksni`). This harness lets a test drive that
condition directly:

- no watcher started  → host ABSENT  (close-to-tray greyed; X quits)
- `start_watcher()`   → host PRESENT (close-to-tray enabled; X hides)
- `stop_watcher()`    → host disappears at runtime (drives `watcher_offine`)

Launch the client with `environment={"DBUS_SESSION_BUS_ADDRESS": bus.address}`;
the linux driver treats a caller-supplied address as "this test owns the bus" and
leaves it alone (`drivers/linux.py::_wants_private_bus`).

The bus itself — a `dbus-daemon` on a private socket performing **no service
activation** — is the general mechanism in `drivers/session_bus.py`, which every
linux launch now gets by default; this class only adds the watcher on top. Read
that module's docstring before changing the bus config: the missing
`<servicedir>` is load-bearing.

The watcher is this process's own child on a private socket, torn down by handle
(never by name) in `stop()`.
"""

import os
import subprocess
import sys
import time

from drivers.port_util import (
    popen_group_kwargs,
    terminate_tree,
    track_process,
    untrack_process,
)
from drivers.session_bus import PrivateSessionBus

_HELPERS_DIR = os.path.dirname(os.path.abspath(__file__))
_FAKE_WATCHER = os.path.join(_HELPERS_DIR, "fake_status_notifier_watcher.py")


class TrayBus(PrivateSessionBus):
    """A private session bus with an optional fake StatusNotifierWatcher."""

    def __init__(self):
        super().__init__()
        self._watcher = None

    # -- watcher (host-present) ----------------------------------------------
    def start_watcher(self, timeout: float = 5.0) -> None:
        """Own `org.kde.StatusNotifierWatcher` on the bus (host becomes present).

        Blocks until the name is owned so the caller can launch the client and
        rely on `watcher_online` firing during the client's tray startup.
        """
        assert self.address, "start() the bus first"
        if self._watcher is not None:
            return
        env = dict(os.environ)
        env["DBUS_SESSION_BUS_ADDRESS"] = self.address
        self._watcher = subprocess.Popen(
            [sys.executable, _FAKE_WATCHER],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            **popen_group_kwargs(),
        )
        track_process(self._watcher)
        deadline = time.monotonic() + timeout
        # Keep every line read: the failure reason (a missing `jeepney`, a bus that
        # rejected the name) arrives on the SAME stream, so discarding what we read
        # while waiting would report a bare '' and hide the traceback that explains it.
        seen: list[str] = []
        while time.monotonic() < deadline:
            line = self._watcher.stdout.readline()
            if line.startswith("OWNED"):
                return
            if line:
                seen.append(line)
            elif self._watcher.poll() is not None:
                break
        try:
            seen.append(self._watcher.stdout.read() or "")
        except Exception:
            pass
        raise RuntimeError(
            "fake StatusNotifierWatcher never owned the name "
            f"(rc={self._watcher.poll()}); output:\n{''.join(seen).strip()!r}"
        )

    def stop_watcher(self) -> None:
        """Release the name (host disappears at runtime → drives `watcher_offine`)."""
        w = self._watcher
        self._watcher = None
        if w is not None:
            terminate_tree(w)
            untrack_process(w)

    # -- teardown -------------------------------------------------------------
    def stop(self) -> None:
        self.stop_watcher()
        super().stop()
