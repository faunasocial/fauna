"""Stale X11 display-lock hygiene for the harness's throwaway Xvfb displays.

Every headless Linux launch (`drivers/linux.py::_headless_render_cmd_env`) runs
fauna-desktop under `xvfb-run -a`, which starts an Xvfb on the first display
number from 99 upward that has no `/tmp/.X<n>-lock`. Xvfb writes that lock (its
own pid) at startup and removes it — together with the `/tmp/.X11-unix/X<n>`
socket — only when it exits cleanly: a SIGKILLed Xvfb leaves both behind
(measured 2026-09-08: TERM removes them, KILL does not). The harness's
teardown paths all end in a KILL — `terminate_tree`'s group-wide TERM → wait →
KILL sweep, and the reaper when a run dies by signal — and 896 leaked locks,
about one per launch, say Xvfb rarely finishes its clean exit first. Nothing
else ever removes a lock in sticky `/tmp`, and `xvfb-run -a` treats every one it
finds as a taken display, so the number it picks only ever climbs: display 99
→ 1464 in two weeks.

The climb is not merely untidy. Mutter starts a GDM *greeter's* Xwayland at
display 1024 and probes 50 numbers upward before giving up, and the greeter
runs as a throwaway uid that cannot unlink another user's lock in sticky
`/tmp` — so every stale lock in 1024–1073 counts as taken. Once the harness's
climb crossed 1024, the greeter's gnome-shell aborted on every GNOME Remote
Login attempt (`Failed to start X Wayland: Failed to create an X lock file:
Gave up after trying to lock different X11 display lock file 50 times`, then
`code=dumped, status=6/ABRT`) and every RDP login showed a black screen with a
cursor and no greeter.

`sweep_stale_x_locks` is the self-healing half, called before each Xvfb launch:
remove every lock this uid owns whose recorded pid no longer exists, plus the
matching socket. Live servers of any uid and other users' files are never
touched — the pid comes from the lock file, never from a name scan, so a
sibling run's in-flight Xvfb is safe (process safety). With the sweep,
`xvfb-run -a` recycles the low numbers and the greeter's range stays clear.
"""

from __future__ import annotations

import os
import re
import stat
from pathlib import Path

_LOCK_NAME = re.compile(r"^\.X(\d+)-lock$")


def sweep_stale_x_locks(
    tmp: str | os.PathLike = "/tmp", *, uid: int | None = None
) -> list[int]:
    """Remove this uid's dead-pid `.X<n>-lock` files under `tmp`, and each one's
    `.X11-unix/X<n>` socket. Returns the display numbers swept, ascending.

    Conservative by construction — a lock is left alone unless ALL of these
    hold: it is a regular file named exactly `.X<n>-lock`; it is owned by `uid`
    (the caller's, by default); its content parses as a pid; and that pid is
    gone (`kill(pid, 0)` → ESRCH — EPERM means a live process we may not
    signal, which is still alive). The socket goes before the lock, so a server
    that claims the freed number can never have its fresh socket removed by a
    sweep that started earlier. Never raises: hygiene must not fail a launch.
    """
    if uid is None:
        getuid = getattr(os, "getuid", None)
        if getuid is None:
            return []
        uid = getuid()
    tmp = Path(tmp)
    try:
        entries = list(tmp.iterdir())
    except OSError:
        return []
    swept: list[int] = []
    for entry in entries:
        match = _LOCK_NAME.match(entry.name)
        if not match:
            continue
        try:
            info = entry.lstat()
        except OSError:
            continue
        if not stat.S_ISREG(info.st_mode) or info.st_uid != uid:
            continue
        try:
            pid = int(entry.read_text(errors="replace").strip())
        except (OSError, ValueError):
            continue
        if pid <= 0 or _pid_alive(pid):
            continue
        display = int(match.group(1))
        try:
            (tmp / ".X11-unix" / f"X{display}").unlink()
        except OSError:
            pass  # never created, already gone, or not ours — the lock is the gate
        try:
            entry.unlink()
        except OSError:
            continue
        swept.append(display)
    return sorted(swept)


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True
