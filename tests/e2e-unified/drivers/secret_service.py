"""A private Secret Service for harness-launched Linux apps: one
`gnome-keyring-daemon` per test, on that test's own private session bus.

`drivers/session_bus.py` closes the box's desktop session bus to every launch
by default. The one launch mode that could not take a private bus was
`use_real_keyring`: its subject is the real Secret Service *implementation*
(libsecret over D-Bus to a real daemon, unlocked-collection semantics, items
that survive a force-quit + relaunch), and until 2026-09-15 the only daemon it
could reach was the **developer's desktop keyring**. That coupling was a
machine-wide hazard, not a test-scoped one: `gnome-keyring-daemon` aborts on a
client that vanishes mid-session (exactly what a force-quit test does), systemd restarts it with the login keyring
**locked**, taking the developer's stored credentials with it.

This class keeps the subject and closes the channel: the installed daemon runs
**for this test alone**, on a private bus, over a throwaway keyring directory,
unlocked non-interactively. The app then speaks real libsecret over real D-Bus
to a real `gnome-keyring-daemon` — the production path — and a crash takes
down only this test's daemon, which `ensure_running()` restarts over the same
directory without losing an item (the login keyring is encrypted at rest with a
password this class knows). The desktop's daemon is never on the bus the app
sees, so it can neither crash nor collect an item.

How the daemon is unlocked without a desktop session (measured 2026-09-15 on
gnome-keyring 50): `--login` reads the login-keyring password from stdin and
creates the collection **unlocked** but leaves the daemon in its initialization
phase; a second `--start` against the same `--control-directory` completes the
initialization and is what makes the daemon claim `org.freedesktop.secrets` on
the bus. `--unlock` against a running daemon does NOT unlock it (it spawns a
second daemon and the collection stays locked). The control directory must be
0700, and `--start` must not run before the daemon has created the control
socket in it.

The daemon is this process's own child, group leader + die-with-parent like
every other harness spawn, torn down by handle in `stop()`.
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
from .session_bus import PrivateSessionBus

#: Password of the throwaway login keyring. Not a secret: the keyring holds one
#: test's fixture identities, under a directory that dies with the test, on a
#: bus only this process and its launches can reach.
KEYRING_PASSWORD = "fauna-e2e"

#: The bus name a Secret Service owns. Polled after `--start` so `start()`
#: returns only once a client can actually reach the collection.
SECRETS_BUS_NAME = "org.freedesktop.secrets"


class PrivateSecretService(PrivateSessionBus):
    """A private session bus owning a private, unlocked `gnome-keyring-daemon`."""

    def __init__(self):
        super().__init__()
        self._state = None
        self._daemon = None
        self._log = None
        #: Exit codes of daemons that died on their own (a negative value is the
        #: signal: -6 is the ABRT the desktop daemon dies of). Empty on a clean
        #: run; a test can assert on it, and `stop()` reports it.
        self.daemon_exits: list[int] = []

    # -- lifecycle ------------------------------------------------------------
    def start(self) -> "PrivateSecretService":
        """Start the bus and the daemon, unlock the login collection, and wait
        until `org.freedesktop.secrets` answers. Idempotent."""
        if self._daemon is not None:
            return self
        for tool in ("gnome-keyring-daemon", "dbus-daemon"):
            if shutil.which(tool) is None:
                raise RuntimeError(
                    f"{tool} not found — linux's real-keyring launches need a private "
                    "Secret Service daemon of their own (install the 'gnome-keyring' "
                    "and 'dbus-daemon'/'dbus' packages for your distribution); gate the "
                    "test on `common.keyring.secret_service_available()` instead of "
                    "letting it reach a desktop keyring"
                )
        super().start()
        self._state = tempfile.mkdtemp(prefix="fauna-e2e-secret-service-")
        for sub in ("home", "data"):
            os.makedirs(os.path.join(self._state, sub), exist_ok=True)
        # 0700 is a hard requirement of the daemon for both, not a courtesy.
        for sub in ("runtime", "control"):
            os.makedirs(os.path.join(self._state, sub), mode=0o700, exist_ok=True)
        self._spawn_daemon()
        return self

    def _daemon_env(self) -> dict:
        """A minimal environment: the private bus, every XDG root under the
        throwaway state dir, and deliberately no `DISPLAY`/`WAYLAND_DISPLAY`,
        so no prompter can reach the desktop even if the daemon wanted one."""
        state = self._state
        return {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "LANG": os.environ.get("LANG", "C.UTF-8"),
            "DBUS_SESSION_BUS_ADDRESS": self.address,
            "HOME": os.path.join(state, "home"),
            "XDG_DATA_HOME": os.path.join(state, "data"),
            "XDG_CONFIG_HOME": os.path.join(state, "home", ".config"),
            "XDG_RUNTIME_DIR": os.path.join(state, "runtime"),
            "GNOME_KEYRING_CONTROL": os.path.join(state, "control"),
        }

    def _spawn_daemon(self) -> None:
        control = os.path.join(self._state, "control")
        env = self._daemon_env()
        # Append, so a restart's output follows the crash it recovers from.
        self._log = open(os.path.join(self._state, "daemon.log"), "ab")
        self._daemon = subprocess.Popen(
            [
                "gnome-keyring-daemon",
                "--foreground",
                "--login",
                "--components=secrets",
                f"--control-directory={control}",
            ],
            stdin=subprocess.PIPE,
            stdout=self._log,
            stderr=subprocess.STDOUT,
            env=env,
            **popen_group_kwargs(),
        )
        track_process(self._daemon)
        self._daemon.stdin.write(KEYRING_PASSWORD.encode() + b"\n")
        self._daemon.stdin.close()

        deadline = time.monotonic() + 10
        while not os.path.exists(os.path.join(control, "control")):
            if self._daemon.poll() is not None:
                raise RuntimeError(
                    "private gnome-keyring-daemon exited before creating its control "
                    f"socket (rc={self._daemon.returncode}); {self._log_tail()}"
                )
            if time.monotonic() > deadline:
                raise RuntimeError(
                    f"private gnome-keyring-daemon never created its control socket; {self._log_tail()}"
                )
            time.sleep(0.02)

        # Completes the initialization `--login` left pending; this is the call
        # that makes the daemon claim the secrets name on the bus.
        started = subprocess.run(
            [
                "gnome-keyring-daemon",
                "--start",
                "--components=secrets",
                f"--control-directory={control}",
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=20,
        )
        if started.returncode != 0:
            raise RuntimeError(
                f"gnome-keyring-daemon --start failed (rc={started.returncode}): "
                f"{started.stderr.strip()}; {self._log_tail()}"
            )

        deadline = time.monotonic() + 10
        last = None
        while time.monotonic() < deadline:
            try:
                if self._collection_unlocked():
                    return
                last = "the default collection is locked"
            except Exception as e:  # not yet on the bus
                last = f"{type(e).__name__}: {e}"
            if self._daemon.poll() is not None:
                raise RuntimeError(
                    f"private gnome-keyring-daemon exited during startup (rc={self._daemon.returncode}); "
                    f"{self._log_tail()}"
                )
            time.sleep(0.05)
        raise RuntimeError(
            f"private Secret Service never became reachable and unlocked ({last}); {self._log_tail()}"
        )

    def _collection_unlocked(self) -> bool:
        import secretstorage

        conn = self.connect(ensure=False)
        try:
            return not secretstorage.get_default_collection(conn).is_locked()
        finally:
            conn.close()

    def _log_tail(self) -> str:
        try:
            with open(os.path.join(self._state, "daemon.log"), "rb") as f:
                tail = f.read()[-800:].decode(errors="replace").strip()
            return f"daemon log tail: {tail!r}"
        except Exception:
            return "no daemon log"

    # -- liveness -------------------------------------------------------------
    def ensure_running(self) -> bool:
        """Restart the daemon if it died, over the same keyring directory — the
        harness's `Restart=on-failure`, with the password systemd lacks, so a
        crash costs nothing persisted. Returns True when a restart happened."""
        if self._daemon is None:
            raise RuntimeError("PrivateSecretService not started")
        rc = self._daemon.poll()
        if rc is None:
            return False
        self.daemon_exits.append(rc)
        untrack_process(self._daemon)
        self._daemon = None
        self._log.close()
        self._spawn_daemon()
        return True

    def connect(self, *, ensure: bool = True):
        """A blocking jeepney connection to this service's bus — what the
        `secretstorage` helpers in `tests/common/keyring.py` operate on. Restarts
        a dead daemon first unless `ensure=False`. Callers close it."""
        from jeepney.io.blocking import open_dbus_connection

        if ensure:
            self.ensure_running()
        return open_dbus_connection(bus=self.address)

    def stop(self) -> None:
        """Terminate the daemon, then the bus, and remove the keyring dir.
        Idempotent."""
        d = self._daemon
        self._daemon = None
        if d is not None:
            rc = d.poll()
            if rc is not None:
                self.daemon_exits.append(rc)
            terminate_tree(d)
            untrack_process(d)
        if self._log is not None:
            self._log.close()
            self._log = None
        super().stop()
        if self._state and os.path.isdir(self._state):
            shutil.rmtree(self._state, ignore_errors=True)
        self._state = None
