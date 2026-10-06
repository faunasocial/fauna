"""POSIX pseudo-terminal backend for the tui driver (`pty`/`fcntl`/`termios`).

The original, primary path — extracted verbatim from `tui.py`'s `launch()` so the
Windows ConPTY backend (`conpty.py`) slots in beside it without touching the
platform-agnostic drain + DA1 logic. Imported only on POSIX
(`pty_backend.spawn_pty` routes by `os.name`), so these POSIX-only stdlib modules
never load on Windows.
"""

import fcntl
import os
import pty
import signal
import struct
import subprocess
import termios

from .port_util import popen_group_kwargs
from .pty_backend import PtyBackend


class PosixPtyBackend(PtyBackend):
    # The winsize ioctl below carries pixel dimensions, so the app sees this
    # terminal's real cell size rather than falling back to an assumed one.
    reports_pixel_size = True

    def __init__(self, cmd, env, rows, cols, cell_w, cell_h, stderr_file):
        master, slave = pty.openpty()
        # A real terminal reports pixel size alongside cells; `fauna-tui` divides
        # the two to size an inline image (`tui.md` § Rendering). Reporting zeroes
        # would make the app fall back to an assumed cell size, silently skipping
        # the real graphics path.
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, cols, cols * cell_w, rows * cell_h),
        )
        self._master = master
        # Frames (stdout) go to the pty; stderr to its own file so a panic message
        # isn't shredded by terminal escapes. `popen_group_kwargs()` makes the child a
        # session/group leader so teardown's `killpg` reaps any helpers it spawned —
        # it was a bare `start_new_session=True` until 2026-08-14, which is the same
        # setsid but WITHOUT PR_SET_PDEATHSIG, so the group survived a SIGKILLed
        # pytest with nothing left to kill it. Same call, strictly stronger guarantee.
        self._proc = subprocess.Popen(
            cmd,
            env=env,
            stdin=slave,
            stdout=slave,
            stderr=stderr_file,
            close_fds=True,
            **popen_group_kwargs(),
        )
        os.close(slave)
        self.pid = self._proc.pid

    def read(self, n: int) -> bytes:
        try:
            return os.read(self._master, n)
        except OSError:
            # Master closed (child gone / teardown) — surface as EOF.
            return b""

    def write(self, data: bytes) -> None:
        os.write(self._master, data)

    def poll(self):
        return self._proc.poll()

    def wait(self, timeout=None):
        return self._proc.wait(timeout=timeout)

    def terminate(self) -> None:
        # The child leads its own session/process group; signal the whole group so
        # any helper it spawned is reaped too. Swallow "already dead".
        try:
            os.killpg(os.getpgid(self._proc.pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError, OSError):
            pass

    def kill(self) -> None:
        try:
            self._proc.kill()
        except (ProcessLookupError, PermissionError, OSError):
            pass

    def resize(self, rows: int, cols: int) -> None:
        fcntl.ioctl(
            self._master,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, cols, 0, 0),
        )

    def close(self) -> None:
        try:
            os.close(self._master)
        except OSError:
            pass
