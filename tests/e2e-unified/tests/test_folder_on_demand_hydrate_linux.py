"""tier_3 — a file another device adds lists in a linux on-demand folder as a
placeholder, and opening it downloads its bytes.

The placeholder half of the linux on-demand root, driven through the apps
(on-demand-files-linux.md § Linux FUSE binding — mount-over, and *hydrate on
open; write through*). `test_folder_on_demand_read_path_linux.py` proves the
switch mounts the root and that other processes read and write through it, but
every file it reads is one the device already held: one seat cannot produce a
file its own device lacks through the UI. Two seats of ONE account on one
machine can (e2e-live-sync-convergence.md — convention 16's shape):

    seat a (the linux app) — bound to the set, turned on-demand with the bound
      row's switch → the kernel's mount table shows `fuse.fauna`
    seat b (a tui seat) — drops a file into its own bound directory
      → on seat a, FROM SUBPROCESSES: `stat` lists the file at its size with NO
        blocks (a placeholder — the bytes are on no disk of this device), and
        the engine's row for it is a `placeholder` holding no local identity
      → `cat` returns seat b's bytes; afterwards the file has blocks and the
        engine's row is `synced` with the served file's identity recorded
      → flip back (UI) → the mount is gone and the directory is an ordinary
        directory holding the file.

**Every verdict is read outside the agent's claims**: the mount table, a child
`ls`/`stat`/`cat`, and the engine's own state DB read-only. Zero blocks is the
FUSE root's own answer for a placeholder (`fuse_host.rs`, a placeholder's
attributes: the row's size, no blocks), so it is the kernel-visible difference
between "listed" and "held" — a hydrated file reports the underlying file's
blocks. No log line witnesses the hydration (the root logs only its failure),
which is why the row is read rather than a log grepped.

The seat trees sit under `/tmp` whatever `TMPDIR` says
(`helpers.folder_content.mountable_location`'s reason: Ubuntu's AppArmor
profile for `fusermount3` admits a mount point only under a short list).
"""

from __future__ import annotations

import secrets
import subprocess

import pytest

from helpers.folder_content import atomic_write, on_demand_mount_stands
from helpers.fuse_mount import run_through_mount
from helpers.waiting import wait_until
from tests.test_filesync_seats import (
    STARTUP_WINDOW,
    WINDOW,
    _seat_run,
    _set_name_for,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

# Ceilings a green run never pays (convention 14): the agent mounts (or
# unmounts) after it has answered the flip, restarting the folder's engine.
MOUNT_WINDOW = 90.0
SEATS = ("native", "tui")


def _run(seat, *argv: str) -> subprocess.CompletedProcess:
    """One short child process through the seat's mount — the 'another
    process' of every read. Bounded even when the agent behind the mount is
    dead (``helpers.fuse_mount``): a hung FUSE request fails the test naming
    the seat's state, never wedges the run."""
    return run_through_mount(seat.path, *argv, diagnose=seat.diagnostics)


def _await_mount(seat, *, mounted: bool) -> None:
    """Poll the kernel's mount table until the on-demand root is (or is no
    longer) mounted over the seat's directory."""
    wait_until(
        lambda: on_demand_mount_stands(seat.path) == mounted, MOUNT_WINDOW, interval=0.3,
        diagnose=lambda: (
            f"the on-demand root was {'never mounted over' if mounted else 'never unmounted from'} "
            f"{seat.path}; error={seat.app.error_text()!r}\n{seat.diagnostics()}"
        ),
    )


def _size_and_blocks(seat, name: str) -> tuple[int, int] | None:
    """``(st_size, st_blocks)`` of ``name`` as another process stats it through
    the mount — ``None`` while the view does not list it. A ``stat`` is a
    lookup, never an open, so it does not hydrate."""
    out = _run(seat, "stat", "-c", "%s %b", str(seat.path / name))
    if out.returncode != 0:
        return None
    size, blocks = out.stdout.decode().split()
    return int(size), int(blocks)


@pytest.mark.timeout(int(len(SEATS) * STARTUP_WINDOW + 3 * WINDOW + 2 * MOUNT_WINDOW + 300))
@pytest.mark.feature("files-on-demand")
def test_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open(
    nest_mode, tmp_path_factory, request
):
    """A file another device adds appears in the on-demand folder by itself,
    costs no disk until it is opened, and opening it serves its bytes."""
    with _seat_run(SEATS, nest_mode, tmp_path_factory, request, seat_dir="/tmp") as run:
        ondemand, other = run.seats
        assert ondemand.app_name == "linux" and other.app_name == "tui", (
            f"seat order drifted: {[s.app_name for s in run.seats]}"
        )
        app = ondemand.app
        set_name = _set_name_for(len(SEATS))

        # MUTATION (UI): the bound row's switch turns seat a's folder on-demand,
        # before any file exists — so whatever arrives later arrives cloud-only.
        b = app.backups
        b.navigate_folders()
        scope = f"folder-row[{b.open_folder(set_name)}]"
        loc = app.sync_locations
        assert loc.mode_toggle_visible(scope=scope), (
            "the bound row must render folder-location-mode-toggle; "
            f"{app.driver.diagnose('folder-location-mode-toggle')} error={app.error_text()!r}"
        )
        assert loc.mode_toggle_state(scope=scope) == "always", (
            "a fresh linux binding starts always-resident (on-demand-files-linux.md § "
            f"Linux FUSE binding); got {loc.mode_toggle_state(scope=scope)!r}"
        )
        assert loc.toggle_mode(scope=scope) == "on-demand", (
            f"the switch did not move the row on-demand; error={app.error_text()!r}\n"
            f"{ondemand.diagnostics()}"
        )
        _await_mount(ondemand, mounted=True)

        # Another device of the account adds a file.
        name = f"{run.run_token}-cloud-only.txt"
        body = f"added on another device {secrets.token_hex(16)}\n".encode()
        atomic_write(other.path / name, body)

        # VERIFICATION, from other processes: the file lists at its size with no
        # bytes on this device's disk.
        wait_until(
            lambda: _size_and_blocks(ondemand, name) is not None, WINDOW, interval=1.0,
            diagnose=lambda: (
                f"{name!r} never listed in the on-demand folder; `ls` reads "
                f"{_run(ondemand, 'ls', '-1a', str(ondemand.path)).stdout!r}\n"
                f"{ondemand.diagnostics()}\n{other.diagnostics()}"
            ),
        )
        listed = _run(ondemand, "ls", "-1", str(ondemand.path)).stdout.decode().split()
        assert name in listed, f"`ls` through the mount must list {name!r}: {listed!r}"
        size, blocks = _size_and_blocks(ondemand, name)
        assert (size, blocks) == (len(body), 0), (
            f"a file this device never opened must list as a placeholder — its "
            f"size ({len(body)}) and no blocks; `stat` reads size={size} blocks={blocks}"
        )
        assert ondemand.sync_entry_states().get(name) == ("placeholder", False), (
            f"the engine's row for {name!r} must be a placeholder holding no local "
            f"identity; rows: {ondemand.sync_entry_states()!r}"
        )

        # Opening it (another process's `cat`) downloads it and serves its bytes.
        read = _run(ondemand, "cat", str(ondemand.path / name))
        assert read.returncode == 0 and read.stdout == body, (
            f"`cat` through the on-demand mount must return the other device's bytes; "
            f"rc={read.returncode} stdout={read.stdout!r} stderr={read.stderr!r}\n"
            f"{ondemand.diagnostics()}"
        )
        # The engine recorded the served file — the record that keeps a
        # downloaded file from reading as a local edit (`SyncEngine::mark_hydrated`)
        # — and the file now has blocks under the view. Both polled: the record
        # lands before the open completes, the kernel's attribute cache may
        # still hold the placeholder's answer for its timeout.
        wait_until(
            lambda: ondemand.sync_entry_states().get(name) == ("synced", True),
            WINDOW, interval=0.5,
            diagnose=lambda: (
                f"the engine never recorded {name!r} as hydrated; rows: "
                f"{ondemand.sync_entry_states()!r}\n{ondemand.diagnostics()}"
            ),
        )
        wait_until(
            lambda: (_size_and_blocks(ondemand, name) or (0, 0))[1] > 0, WINDOW, interval=0.5,
            diagnose=lambda: (
                f"{name!r} still reports no blocks after it was opened: "
                f"{_size_and_blocks(ondemand, name)!r}"
            ),
        )

        # MUTATION (UI): and back. The mount comes down and the hydrated file is
        # an ordinary file in the user's directory.
        b.navigate_folders()
        scope = f"folder-row[{b.open_folder(set_name)}]"
        assert loc.toggle_mode(scope=scope) == "always", (
            f"the switch did not move the row back to always; error={app.error_text()!r}\n"
            f"{ondemand.diagnostics()}"
        )
        _await_mount(ondemand, mounted=False)
        assert (ondemand.path / name).read_bytes() == body
        assert not app.has_error(), f"the round trip surfaced an error: {app.error_text()!r}"
