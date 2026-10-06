"""Unit tests for `drivers/x_display.py::sweep_stale_x_locks` — the harness's
own `/tmp` hygiene for the throwaway Xvfb displays it launches fauna-desktop on.

Why a test-suite needs to clean up after its display server: a SIGKILLed Xvfb
leaves its `/tmp/.X<n>-lock` behind (measured: SIGTERM removes lock + socket,
SIGKILL leaves both), the group-wide TERM → KILL teardown ends nearly every
launch that way, and `xvfb-run -a` treats each leftover lock as a taken
display — so the display number only ever climbs. On a Linux development box
it climbed from 99 past 1024 in two weeks, and 1024 upward is where mutter
starts the GDM greeter's Xwayland: the greeter, a throwaway uid that cannot
unlink another user's files in sticky `/tmp`, gave up after 50 taken displays
and every GNOME Remote Login attempt showed a black screen with no greeter.
The sweep keeps the climb from ever starting. Each case below pins one edge of
its conservatism — it must remove only what is provably dead and ours.
"""

import os
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = [pytest.mark.tier_1, pytest.mark.skipif(os.name != "posix", reason="POSIX-only")]
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.port_util import popen_group_kwargs
from drivers.x_display import sweep_stale_x_locks


def _dead_pid() -> int:
    """A pid that certainly is not running: a child of ours, already reaped."""
    proc = subprocess.Popen(["true"], **popen_group_kwargs())
    proc.wait()
    return proc.pid


def _write_lock(tmp: Path, display: int, pid) -> Path:
    # The X server's own lock format: the pid right-aligned in 10 columns plus
    # a newline (11 bytes) — what `sweep_stale_x_locks` must parse.
    lock = tmp / f".X{display}-lock"
    lock.write_text(pid if isinstance(pid, str) else f"{pid:>10d}\n")
    return lock


def _write_socket(tmp: Path, display: int) -> Path:
    # A plain file stands in for the unix socket; the sweep only unlinks it.
    sock_dir = tmp / ".X11-unix"
    sock_dir.mkdir(exist_ok=True)
    sock = sock_dir / f"X{display}"
    sock.touch()
    return sock


class TestSweepStaleXLocks:
    def test_dead_pid_lock_and_its_socket_are_removed(self, tmp_path):
        lock = _write_lock(tmp_path, 1030, _dead_pid())
        sock = _write_socket(tmp_path, 1030)

        assert sweep_stale_x_locks(tmp_path) == [1030]
        assert not lock.exists()
        assert not sock.exists()

    def test_live_pid_lock_is_kept(self, tmp_path):
        # Our own pid: alive by construction for the duration of the test.
        lock = _write_lock(tmp_path, 1031, os.getpid())
        sock = _write_socket(tmp_path, 1031)

        assert sweep_stale_x_locks(tmp_path) == []
        assert lock.exists()
        assert sock.exists()

    def test_unparsable_lock_is_kept(self, tmp_path):
        # Not a pid → cannot prove the server is dead → not ours to remove.
        lock = _write_lock(tmp_path, 1032, "not-a-pid\n")

        assert sweep_stale_x_locks(tmp_path) == []
        assert lock.exists()

    def test_lock_owned_by_another_uid_is_kept(self, tmp_path):
        # Seen from a sweeper running as a DIFFERENT uid, our file is foreign:
        # a stale lock of another user is theirs to clean, never ours.
        lock = _write_lock(tmp_path, 1033, _dead_pid())

        assert sweep_stale_x_locks(tmp_path, uid=os.getuid() + 1) == []
        assert lock.exists()

    def test_only_lock_files_are_considered(self, tmp_path):
        # Neighbours in /tmp that merely look similar must be left alone.
        decoy = tmp_path / ".X1034-lock.bak"
        decoy.write_text(f"{_dead_pid():>10d}\n")
        other = tmp_path / "X1034-lock"
        other.write_text(f"{_dead_pid():>10d}\n")

        assert sweep_stale_x_locks(tmp_path) == []
        assert decoy.exists()
        assert other.exists()

    def test_missing_tmp_dir_is_a_noop(self, tmp_path):
        assert sweep_stale_x_locks(tmp_path / "nowhere") == []

    def test_mixed_directory_sweeps_exactly_the_dead_ones(self, tmp_path):
        dead_a = _write_lock(tmp_path, 100, _dead_pid())
        live = _write_lock(tmp_path, 101, os.getpid())
        dead_b = _write_lock(tmp_path, 1050, _dead_pid())
        _write_socket(tmp_path, 1050)

        assert sweep_stale_x_locks(tmp_path) == [100, 1050]
        assert not dead_a.exists()
        assert live.exists()
        assert not dead_b.exists()
        assert not (tmp_path / ".X11-unix" / "X1050").exists()
