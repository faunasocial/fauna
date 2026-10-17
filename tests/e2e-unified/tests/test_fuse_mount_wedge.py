"""tier_1 harness test: **a read through a FUSE mount whose daemon never
answers fails within its window** (``e2e-conventions.md`` convention 9 — the
harness is self-terminating), through ``helpers.fuse_mount``.

The daemon is a few dozen lines of the FUSE wire protocol over ``/dev/fuse``,
mounted by ``fusermount3`` exactly the way libfuse mounts (the ``_FUSE_COMMFD``
socket hand-off), run on a thread of this process: it answers ``INIT``, the
lookup, the attributes and the open of one file, and then never answers the
``READ`` — the state a dead-but-unreaped or wedged sync agent leaves its
on-demand root in. A reader that has sent that ``READ`` sleeps uninterruptibly,
SIGKILL included, which is how one ``cat`` once held a full linux run for
~30 h.

Two pins: the hazard itself (``subprocess.run``'s own timeout does NOT bound
such a read — a watchdog has to abort the connection to end it), and the
helper (``run_through_mount`` fails inside its window, names the dead daemon,
and reaps the reader).
"""

from __future__ import annotations

import errno
import os
import shutil
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import pytest

from helpers.fuse_mount import (
    FuseMountWedged,
    fuse_connection,
    mount_type,
    release_mount,
    run_through_mount,
)

pytestmark = [
    pytest.mark.tier_1,
    pytest.mark.skipif(
        not sys.platform.startswith("linux"), reason="FUSE mounts are a linux harness concern"
    ),
]

NAME = "wedged.txt"
SUBTYPE = "faunawedge"

# FUSE opcodes (include/uapi/linux/fuse.h).
_LOOKUP, _FORGET, _GETATTR, _OPEN, _READ = 1, 2, 3, 14, 15
_RELEASE, _FLUSH, _INIT, _INTERRUPT, _DESTROY = 18, 25, 26, 36, 38
_BATCH_FORGET = 42
_NO_REPLY = {_FORGET, _BATCH_FORGET, _INTERRUPT}
_IN_HEADER = struct.Struct("<IIQQIIIHH")  # len opcode unique nodeid uid gid pid extlen pad
_OUT_HEADER = struct.Struct("<IiQ")       # len error unique


def _attr(ino: int, mode: int, size: int) -> bytes:
    # fuse_attr: ino size blocks atime mtime ctime (u64) · atimensec mtimensec
    # ctimensec mode nlink uid gid rdev blksize flags (u32).
    nlink = 2 if stat.S_ISDIR(mode) else 1
    return struct.pack(
        "<QQQQQQIIIIIIIIII", ino, size, 0, 0, 0, 0, 0, 0, 0, mode, nlink,
        os.getuid(), os.getgid(), 0, 4096, 0,
    )


_ROOT = _attr(1, stat.S_IFDIR | 0o755, 0)
_FILE = _attr(2, stat.S_IFREG | 0o644, 5)


class WedgedDaemon:
    """A FUSE daemon that serves one file and never answers a READ of it."""

    def __init__(self, mountpoint: Path):
        self.mountpoint = mountpoint
        self.reads_held = threading.Event()
        ours, theirs = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        with ours, theirs:
            env = dict(os.environ, _FUSE_COMMFD=str(theirs.fileno()))
            mounted = subprocess.run(
                ["fusermount3", "-o", f"subtype={SUBTYPE}", "--", str(mountpoint)],
                env=env, pass_fds=(theirs.fileno(),), capture_output=True, timeout=30,
            )
            if mounted.returncode != 0:
                pytest.fail(f"fusermount3 could not mount {mountpoint}: {mounted.stderr!r}")
            _msg, fds, _flags, _addr = socket.recv_fds(ours, 16, 1)
        self.fd = fds[0]
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _reply(self, unique: int, body: bytes = b"", error: int = 0) -> None:
        os.write(self.fd, _OUT_HEADER.pack(_OUT_HEADER.size + len(body), -error, unique) + body)

    def _serve(self) -> None:
        while True:
            try:
                request = os.read(self.fd, 1 << 18)
            except OSError as e:  # ENODEV once the connection is aborted
                if e.errno == errno.EINTR:
                    continue
                return
            _len, opcode, unique, nodeid, *_ = _IN_HEADER.unpack_from(request)
            arg = request[_IN_HEADER.size:]
            if opcode in _NO_REPLY:
                continue
            if opcode == _INIT:
                major, minor, readahead, flags = struct.unpack_from("<IIII", arg)
                # fuse_init_out: major minor max_readahead flags · max_background
                # congestion_threshold · max_write time_gran · max_pages
                # map_alignment · flags2 max_stack_depth · unused[6].
                self._reply(unique, struct.pack(
                    "<IIIIHHIIHHII6I", 7, 31, readahead, 0, 16, 12, 4096, 1, 0, 0, 0, 0,
                    *([0] * 6),
                ))
            elif opcode == _GETATTR:
                attr = _ROOT if nodeid == 1 else _FILE
                self._reply(unique, struct.pack("<QII", 0, 0, 0) + attr)
            elif opcode == _LOOKUP:
                if nodeid == 1 and arg.rstrip(b"\0") == NAME.encode():
                    self._reply(unique, struct.pack("<QQQQII", 2, 0, 0, 0, 0, 0) + _FILE)
                else:
                    self._reply(unique, error=errno.ENOENT)
            elif opcode == _OPEN:
                self._reply(unique, struct.pack("<QII", 0, 0, 0))
            elif opcode == _READ:
                self.reads_held.set()  # sent, and never answered
            elif opcode in (_RELEASE, _FLUSH, _DESTROY):
                self._reply(unique)
            else:
                self._reply(unique, error=errno.ENOSYS)

    def close(self) -> None:
        released = release_mount(self.mountpoint)
        os.close(self.fd)
        self.thread.join(timeout=10)
        assert released, f"the test's FUSE mount still stands over {self.mountpoint}"


@pytest.fixture
def wedged_mount():
    if shutil.which("fusermount3") is None or not os.path.exists("/dev/fuse"):
        pytest.fail("this linux box has no fusermount3 or /dev/fuse — the on-demand root needs both")
    # Under /tmp itself: Ubuntu's AppArmor profile for fusermount3 admits a
    # mount point only under a short list (helpers.folder_content.mountable_location).
    mountpoint = Path(tempfile.mkdtemp(prefix="fauna-e2e-fuse-wedge-", dir="/tmp"))
    daemon = WedgedDaemon(mountpoint)
    try:
        assert mount_type(mountpoint) == f"fuse.{SUBTYPE}"
        assert fuse_connection(mountpoint) is not None, (
            f"no fusectl connection for {mountpoint} — is /sys/fs/fuse/connections mounted?"
        )
        yield daemon
    finally:
        daemon.close()
        shutil.rmtree(mountpoint, ignore_errors=True)


@pytest.mark.timeout(60)
def test_subprocess_run_timeout_does_not_bound_a_read_the_daemon_never_answers(wedged_mount):
    """The hazard, pinned so nobody "simplifies" the helper back to
    ``subprocess.run``: its timeout kills the reader and then waits for a
    process the kernel will not let go. Only the abort ends the wait."""
    from helpers.fuse_mount import abort_fuse_connection

    watchdog_fired = threading.Event()

    def _watchdog():
        if not wedged_mount.reads_held.wait(20):
            return
        time.sleep(8)  # sleep-ok: the pin is that a 2 s read timeout does NOT end the wait; the abort must come well after it
        watchdog_fired.set()
        abort_fuse_connection(wedged_mount.mountpoint)

    threading.Thread(target=_watchdog, daemon=True).start()
    started = time.monotonic()
    with pytest.raises(subprocess.TimeoutExpired):
        subprocess.run(["cat", str(wedged_mount.mountpoint / NAME)], capture_output=True, timeout=2)
    elapsed = time.monotonic() - started
    assert watchdog_fired.is_set(), (
        f"subprocess.run returned after {elapsed:.1f}s without the abort — the kernel "
        "no longer holds a sent FUSE request through SIGKILL, so this pin (and the "
        "helper's reason) needs re-reading"
    )


@pytest.mark.timeout(60)
def test_a_read_through_a_wedged_mount_fails_within_its_window(wedged_mount):
    started = time.monotonic()
    with pytest.raises(FuseMountWedged) as failure:
        run_through_mount(
            wedged_mount.mountpoint, "cat", str(wedged_mount.mountpoint / NAME),
            timeout=3, diagnose=lambda: "seat diagnostics: the agent is gone",
        )
    elapsed = time.monotonic() - started
    assert wedged_mount.reads_held.is_set(), "the reader never reached the daemon's READ"
    assert elapsed < 3 + 10, f"the bounded read took {elapsed:.1f}s"
    message = str(failure.value)
    assert "dead or wedged" in message and "connection aborted" in message, message
    assert "the child reaped" in message, message
    assert "seat diagnostics: the agent is gone" in message, message
    # The abort is sticky: the next touch fails fast instead of hanging.
    again = run_through_mount(wedged_mount.mountpoint, "ls", str(wedged_mount.mountpoint), timeout=5)
    assert again.returncode != 0 and b"not connected" in again.stderr, again


def test_a_read_through_a_live_mount_returns_its_result(wedged_mount):
    """The helper is a plain ``subprocess.run`` while the daemon answers."""
    listed = run_through_mount(
        wedged_mount.mountpoint, "stat", "-c", "%s", str(wedged_mount.mountpoint / NAME),
        text=True, timeout=10,
    )
    assert listed.returncode == 0 and listed.stdout.strip() == "5", listed
