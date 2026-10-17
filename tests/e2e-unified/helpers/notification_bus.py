"""A private session bus with a Secret Service and a fake notification daemon,
for a linux sync agent's push arm under test.

The linux agent's banner sink (`bins/fauna-sync-agent/src/push_arm.rs`,
`DesktopSink`) needs two things from its session bus:

- **a usable keyring** — the sink counts as present only when
  `fauna_credential_store::keyring_probe` finds an unlocked default collection
  (the "a desktop session is here" selector). Without one every frame tallies
  `no_sink`. The base class, `drivers/secret_service.py::PrivateSecretService`,
  gives the bus a private, unlocked `gnome-keyring-daemon`.
- **an `org.freedesktop.Notifications` owner** — `notify-rust`'s `Notify` call.
  Without one the post errors and the frame tallies `failed`. This class adds
  `helpers/fake_notification_daemon.py`, which records every `Notify` so the
  test reads back what was posted — the linux twin of windows reading the
  toast history.

Spawn the agent with ``DBUS_SESSION_BUS_ADDRESS=bus.address``. Nothing here
touches the box's desktop session bus, its keyring or its notification daemon
(convention 10), so a posted banner never reaches a screen.

The daemon is this process's own child, torn down by handle in `stop()`.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time

from drivers.port_util import (
    popen_group_kwargs,
    terminate_tree,
    track_process,
    untrack_process,
)
from drivers.secret_service import PrivateSecretService

_HELPERS_DIR = os.path.dirname(os.path.abspath(__file__))
_FAKE_DAEMON = os.path.join(_HELPERS_DIR, "fake_notification_daemon.py")


class NotificationBus(PrivateSecretService):
    """A private session bus owning an unlocked keyring and a recording
    `org.freedesktop.Notifications`."""

    def __init__(self):
        super().__init__()
        self._notifier = None
        self._reader = None
        self._lock = threading.Lock()
        self._posted: list[dict] = []
        self._other_output: list[str] = []

    def start(self, timeout: float = 5.0) -> "NotificationBus":
        """Start the bus, the keyring and the notification daemon; return once
        the daemon owns its name. Idempotent."""
        super().start()
        if self._notifier is not None:
            return self
        env = dict(os.environ)
        env["DBUS_SESSION_BUS_ADDRESS"] = self.address
        self._notifier = subprocess.Popen(
            [sys.executable, _FAKE_DAEMON],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            **popen_group_kwargs(),
        )
        track_process(self._notifier)
        deadline = time.monotonic() + timeout
        # Every line read is kept: a failure's reason (a missing `jeepney`, a bus
        # that refused the name) arrives on the same stream.
        seen: list[str] = []
        while time.monotonic() < deadline:
            line = self._notifier.stdout.readline()
            if line.startswith("OWNED"):
                # Convention 13: the pipe is drained for the child's whole life.
                self._reader = threading.Thread(
                    target=self._drain, name="fake-notifications-drain", daemon=True
                )
                self._reader.start()
                return self
            if line:
                seen.append(line)
            elif self._notifier.poll() is not None:
                break
        try:
            seen.append(self._notifier.stdout.read() or "")
        except Exception:
            pass
        rc = self._notifier.poll()
        self.stop()
        raise RuntimeError(
            "fake org.freedesktop.Notifications never owned the name "
            f"(rc={rc}); output:\n{''.join(seen).strip()!r}"
        )

    def _drain(self) -> None:
        for line in self._notifier.stdout:
            with self._lock:
                if line.startswith("NOTIFY "):
                    self._posted.append(json.loads(line[len("NOTIFY "):]))
                else:
                    self._other_output.append(line)

    def notifications(self) -> list[dict]:
        """Every `Notify` the daemon answered so far, oldest first:
        ``{id, app_name, summary, body, actions}``."""
        with self._lock:
            return [dict(n) for n in self._posted]

    def daemon_output(self) -> str:
        """Whatever else the daemon printed — a traceback, for a diagnosis."""
        with self._lock:
            return "".join(self._other_output)

    def stop(self) -> None:
        n = self._notifier
        self._notifier = None
        if n is not None:
            terminate_tree(n)
            untrack_process(n)
        super().stop()
