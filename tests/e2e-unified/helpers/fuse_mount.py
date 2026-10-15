"""Bounded access through a test-owned **FUSE mount** — the linux on-demand
root (``on-demand-files-linux.md`` § Linux FUSE binding) is one.

Why this exists (``e2e-conventions.md`` convention 9, the harness is
self-terminating): a request a FUSE daemon has been sent but never answers
leaves the reading process in uninterruptible sleep (``D`` state, ``wchan``
``request_wait_answer``) — SIGKILL included, the kernel waits for the answer.
``subprocess.run(..., timeout=N)`` kills the child on timeout and then WAITS
for it, so a wedged or half-dead daemon turns a bounded read into an unbounded
one, past pytest-timeout too: a full linux run once held the machine's only
long e2e slot for ~30 h on one ``cat``.

The release is the kernel's own: writing ``1`` to the connection's
``/sys/fs/fuse/connections/<dev>/abort`` fails every pending and future
request with ``ENOTCONN``, which wakes the waiter so it can be reaped. The
control files belong to the mounting user, so the harness needs no privilege
for its own mounts. Every child that touches a FUSE mount goes through
``run_through_mount``; every teardown that leaves a mount standing over a
directory it is about to remove goes through ``release_mount``.
"""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Callable

from drivers.port_util import popen_group_kwargs, reap_descendants_of

# A ceiling a green read never pays (convention 14): one short child process.
FUSE_READ_WINDOW = 120.0
# How long the reaper waits for a woken reader after the abort. A reader that
# outlives it is reported, never waited for.
_REAP_WINDOW = 10.0

_CONNECTIONS = Path("/sys/fs/fuse/connections")


class FuseMountWedged(AssertionError):
    """A child touching a FUSE mount outlived its window: the mount's daemon is
    dead or wedged. An ``AssertionError`` so the test fails (never errors), and
    so ``wait_until`` — which retries only a falsy predicate — stops at once."""


def _mount_entry(location: Path) -> tuple[str, str] | None:
    """``(major:minor, fstype)`` of the mount whose mount point is exactly
    ``location`` (the topmost one when several stack), from the kernel's own
    table — ``None`` when nothing is mounted there."""
    try:
        table = Path("/proc/self/mountinfo").read_text()
    except OSError:
        return None
    target = str(location)
    found = None
    for line in table.splitlines():
        left, _, right = line.partition(" - ")
        fields = left.split()
        # mountinfo escapes a space in the mount point as `\040`.
        if len(fields) > 4 and fields[4].replace("\\040", " ") == target:
            found = (fields[2], (right.split() or [""])[0])
    return found


def mount_type(location: Path) -> str | None:
    """The filesystem type mounted at ``location`` (``fuse.fauna`` for the
    agent's on-demand root), or ``None``."""
    entry = _mount_entry(location)
    return entry[1] if entry else None


def fuse_connection(location: Path) -> Path | None:
    """The ``fusectl`` directory of the FUSE connection mounted at
    ``location``. It is named by the superblock's device number, which for a
    FUSE mount is an anonymous device (major 0) — so the name is the minor."""
    entry = _mount_entry(location)
    if entry is None or not (entry[1] == "fuse" or entry[1].startswith("fuse.")):
        return None
    major, _, minor = entry[0].partition(":")
    if major != "0":
        return None
    connection = _CONNECTIONS / minor
    return connection if connection.is_dir() else None


def _waiting(connection: Path | None) -> str:
    try:
        return (connection / "waiting").read_text().strip() if connection else "?"
    except OSError:
        return "?"


def abort_fuse_connection(location: Path) -> bool:
    """Abort the FUSE connection mounted at ``location``: every request its
    daemon has not answered fails with ``ENOTCONN``, and so does every later
    one, until the mount is unmounted. ``True`` when the abort was written."""
    connection = fuse_connection(location)
    if connection is None:
        return False
    try:
        (connection / "abort").write_text("1")
    except OSError:
        return False
    return True


def run_through_mount(
    mount: Path,
    *argv: str,
    text: bool = False,
    timeout: float = FUSE_READ_WINDOW,
    diagnose: Callable[[], str] | None = None,
) -> subprocess.CompletedProcess:
    """Run one short child that touches ``mount`` — ``subprocess.run`` with
    ``capture_output=True`` whose timeout HOLDS when the mount's daemon does
    not answer: on timeout the mount's connection is aborted BEFORE the child
    is reaped, and the test fails naming the dead daemon. ``diagnose`` (the
    seat's or the agent's state) is called only then."""
    child = subprocess.Popen(
        argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=text,
        **popen_group_kwargs(),
    )
    reap_descendants_of(child.pid)
    try:
        stdout, stderr = child.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        connection = fuse_connection(mount)
        waiting = _waiting(connection)
        aborted = abort_fuse_connection(mount)
        child.kill()
        try:
            child.communicate(timeout=_REAP_WINDOW)
            reaped = "reaped"
        except subprocess.TimeoutExpired:
            reaped = f"STILL UNREAPED (pid {child.pid}) — left behind, never waited for"
        detail = f"\n{diagnose()}" if diagnose is not None else ""
        raise FuseMountWedged(
            f"{argv!r} through the FUSE mount at {mount} got no answer within "
            f"{timeout:.0f}s: the mount's daemon is dead or wedged "
            f"({waiting} request(s) waiting on {connection or 'no fusectl connection'}; "
            f"connection {'aborted' if aborted else 'NOT aborted — no fusectl entry'}; "
            f"the child {reaped}){detail}"
        ) from None
    return subprocess.CompletedProcess(argv, child.returncode, stdout, stderr)


def release_mount(location: Path) -> bool:
    """Bring down a FUSE mount still standing over ``location`` at teardown,
    whatever state its daemon is in: abort the connection (a dead daemon
    otherwise wedges whoever touches the directory next — the rmtree first),
    then lazily unmount it. ``True`` when nothing is mounted there afterwards.
    Only for a mount whose daemon this test owns and has already stopped:
    aborting a live root takes it away from its agent."""
    if fuse_connection(location) is None:
        return mount_type(location) is None
    abort_fuse_connection(location)
    try:
        subprocess.run(
            ["fusermount3", "-u", "-z", str(location)],
            capture_output=True, timeout=_REAP_WINDOW,
        )
    except (OSError, subprocess.TimeoutExpired):
        pass
    return mount_type(location) is None
