"""The two-seat on-demand hydrate journey — one journey, one host probe per OS.

`docs/features/files-on-demand.md` outcomes 11 (*a file this device has not
opened takes no space, and opening it downloads its bytes*) and 12 (*a file added
from another of your devices shows up in your on-demand folder without you doing
anything*) are one promise on every host that serves an on-demand root over a
bound location: hydrate on open is the shared mechanism
(`docs/goal/behavior/on-demand-files.md` § On-Demand Files, the shared hydration
primitive), and only the root's surface differs — a FUSE mount on linux
(`on-demand-files-linux.md` § Linux FUSE binding), a cloud-files sync root on
windows (`docs/goal/architecture/apps/windows.md` § Shell Extension). So the
journey is written once, here, and each host module passes the probe that reads
its root from outside the agent:

    seat a (the host's own app, or tui) — bound to the set, its row's switch on
      on-demand (flipped there on a host whose fresh binding starts always)
      → the root stands over the seat's directory
    seat b (a tui seat) — drops a file into its own bound directory
      → on seat a, read from OUTSIDE the agent: the file lists at its size while
        its bytes are on no disk of this device, and the engine's row for it is
        a `placeholder` holding no local identity
      → another process reads it and gets seat b's bytes; afterwards the file
        is held and the engine's row is `synced` with the served file's
        identity recorded
      → flip back (UI) → the root is gone and the directory holds an ordinary
        file with those bytes.

**Every verdict is read outside the agent's claims** — the agent is the
provider, and a provider's own read of its own placeholder fires no fetch at all
(`on-demand-files.md` § A placeholder is present-but-unreadable, *Never read
one*). The probes read the root's registration, the file's residency and its
bytes from the test process or a child of it, and the engine's state DB
read-only (:meth:`AppSeatDriver.sync_entry_states`). No log line witnesses a
hydration, which is why the row is read rather than a log grepped.
"""

from __future__ import annotations

import os
import secrets
import subprocess
import sys
from typing import NamedTuple

import pytest

from helpers.folder_content import (
    atomic_write,
    on_demand_mount_stands,
    on_demand_root_stands,
    open_bound_row,
)
from helpers.seat_modes import native_app_for_platform
from helpers.waiting import wait_until

# A ceiling a green run never pays (convention 14): the agent mounts (or
# registers) the root after it has answered the flip, restarting the folder's
# engine in between.
ROOT_WINDOW = 90.0

# A cfapi fetch nobody answers fails in the child at the platform's own 60 s
# recall timeout (`ERROR_CLOUD_FILE_REQUEST_TIMEOUT`); this ceiling sits above it
# so that failure reaches the test with its own stderr rather than as a kill.
_CLOUD_FILES_READ_WINDOW = 90.0


class Residency(NamedTuple):
    """One file as a process other than the agent sees it."""

    size: int
    #: True while the file's bytes are on no disk of this device.
    cloud_only: bool
    #: The raw observation, for a failure message (convention 6).
    detail: str


class FuseHost:
    """linux — the agent's FUSE root mounted over the bound directory.

    Every probe is a short child through the mount
    (:func:`helpers.fuse_mount.run_through_mount`), bounded even when the agent
    behind the mount is dead. Zero blocks is the FUSE root's own answer for a
    placeholder (`fuse_host.rs`, a placeholder's attributes: the row's size, no
    blocks), so it is the kernel-visible difference between "listed" and
    "held"; a hydrated file reports the underlying file's blocks. A ``stat`` is
    a lookup, never an open, so it does not hydrate.
    """

    root_noun = "FUSE mount"
    fresh_mode = "always"
    fresh_mode_doc = "on-demand-files-linux.md § Linux FUSE binding"
    #: Ubuntu's AppArmor profile for `fusermount3` admits a mount point only
    #: under a short list of directories
    #: (`helpers.folder_content.mountable_location` says which).
    seat_dir = "/tmp"

    def root_stands(self, seat) -> bool:
        return on_demand_mount_stands(seat.path)

    def _run(self, seat, *argv: str) -> subprocess.CompletedProcess:
        from helpers.fuse_mount import run_through_mount

        return run_through_mount(seat.path, *argv, diagnose=seat.diagnostics)

    def listing(self, seat) -> list[str] | None:
        out = self._run(seat, "ls", "-1a", str(seat.path))
        return out.stdout.decode().split() if out.returncode == 0 else None

    def residency(self, seat, name: str) -> Residency | None:
        out = self._run(seat, "stat", "-c", "%s %b", str(seat.path / name))
        if out.returncode != 0:
            return None
        size, blocks = (int(v) for v in out.stdout.decode().split())
        return Residency(size, blocks == 0, f"stat: size={size} blocks={blocks}")

    def read(self, seat, name: str) -> subprocess.CompletedProcess:
        return self._run(seat, "cat", str(seat.path / name))


class CloudFilesHost:
    """windows — the agent's cloud-files sync root registered over the bound
    directory.

    The root's registration is the filter's own answer
    (:func:`helpers.folder_content.on_demand_root_stands`). Residency is the
    OS's placeholder attribute — a cloud-only placeholder carries
    ``FILE_ATTRIBUTE_OFFLINE`` / ``FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS``, *"which
    a hydrated file no longer carries"* (`on-demand-files.md` § A placeholder is
    present-but-unreadable) — read by the tier_1-pinned reader
    (:mod:`helpers.windows_placeholder`) in this test process, which is not the
    provider; the size comes off the directory listing. Neither opens the
    file's data, so neither hydrates. The read is ``cmd /c type`` in a child:
    the fetch a shell open raises, from a process that exits.
    """

    root_noun = "cloud-files sync root"
    fresh_mode = "on-demand"
    fresh_mode_doc = "on-demand-files.md § On-Demand Files, *The choice is the user's*"
    seat_dir = None

    def root_stands(self, seat) -> bool:
        return on_demand_root_stands(seat.path)

    def listing(self, seat) -> list[str] | None:
        try:
            return sorted(os.listdir(seat.path))
        except OSError:
            return None

    def residency(self, seat, name: str) -> Residency | None:
        from helpers import windows_placeholder as ph

        try:
            with os.scandir(seat.path) as entries:
                entry = next((e for e in entries if e.name == name), None)
            if entry is None:
                return None
            # The listing's own size: a DirEntry's stat on windows is the find
            # data, never an open of the file.
            size = entry.stat().st_size
            cloud_only = ph.is_placeholder(seat.path / name)
        except OSError:  # FileNotFoundError included: gone between the two reads
            return None
        return Residency(size, cloud_only, f"{ph.describe(seat.path / name)} size={size}")

    def read(self, seat, name: str) -> subprocess.CompletedProcess:
        from helpers import windows_placeholder as ph

        path = seat.path / name
        try:
            return ph.hydrate_via_shell(
                path, timeout=_CLOUD_FILES_READ_WINDOW, capture_stdout=True
            )
        except subprocess.TimeoutExpired:
            pytest.fail(
                f"`cmd /c type {path}` did not return within "
                f"{_CLOUD_FILES_READ_WINDOW:.0f}s — past the platform's own recall "
                f"timeout, so the fetch never reached a provider; "
                f"{ph.describe(path)}\n{seat.diagnostics()}"
            )


FUSE_HOST = FuseHost()
CLOUD_FILES_HOST = CloudFilesHost()


def seat_app_names(seat_set) -> tuple[str, ...]:
    """The app each seat of ``seat_set`` drives on this platform."""
    return tuple(
        native_app_for_platform(sys.platform) if mode == "native" else mode
        for mode in seat_set
    )


def seat_set_param(seat_set):
    """``seat_set`` as the test's ``seat_set`` parameter, under the seat
    modules' id (``2seat-native+tui``).

    A parameter, never a constant in the body: the harness reads a seat set
    only off an item's parametrization (`conftest._apps_for_seat_pair`), so a
    constant leaves the helper seat's app to build lazily, after seat a has
    started. On windows that lazy build cannot replace
    ``fauna-sync-agent.exe`` while seat a's agent runs from it, and the run
    fails before the journey begins (measured 2026-11-02 on Windows: `tui-debug`
    failed twice, "failed to remove file …\\fauna-sync-agent.exe")."""
    from helpers.sync_seats import seat_set_param_id

    return pytest.param(seat_set, id=seat_set_param_id(seat_set))


def _await_root(host, seat, *, standing: bool) -> None:
    """Poll the host's own answer until the on-demand root stands over the
    seat's directory (or no longer does)."""
    wait_until(
        lambda: host.root_stands(seat) == standing, ROOT_WINDOW, interval=0.3,
        diagnose=lambda: (
            f"the on-demand {host.root_noun} "
            f"{'never stood over' if standing else 'never came down from'} {seat.path}; "
            f"error={seat.app.error_text()!r}\n{seat.diagnostics()}"
        ),
    )


def hydrate_journey(run, host, *, seat_set, set_name: str, window: float) -> None:
    """The journey on seat a of ``run`` (a ``tests.test_filesync_seats.SeatRun``
    over ``seat_set``), whichever app sits there (module docstring)."""
    ondemand, other = run.seats
    assert (ondemand.app_name, other.app_name) == seat_app_names(seat_set), (
        f"seat order drifted: {[s.app_name for s in run.seats]}"
    )
    app = ondemand.app
    loc = app.sync_locations

    # The bound row's switch, before any file exists — so whatever arrives
    # later arrives cloud-only. A host whose fresh binding starts always is
    # turned on-demand here (MUTATION, UI); one that starts on-demand owes no
    # flip, and reading the switch is the check that it did.
    scope = open_bound_row(app, set_name, diagnose=ondemand.diagnostics)
    fresh = loc.mode_toggle_state(scope=scope)
    assert fresh == host.fresh_mode, (
        f"a fresh {ondemand.app_name} binding on this host starts {host.fresh_mode!r} "
        f"({host.fresh_mode_doc}); got {fresh!r}"
    )
    if fresh != "on-demand":
        assert loc.toggle_mode(scope=scope) == "on-demand", (
            f"the switch did not move the row on-demand; error={app.error_text()!r}\n"
            f"{ondemand.diagnostics()}"
        )
    _await_root(host, ondemand, standing=True)

    # Another device of the account adds a file.
    name = f"{run.run_token}-cloud-only.txt"
    body = f"added on another device {secrets.token_hex(16)}\n".encode()
    atomic_write(other.path / name, body)

    # VERIFICATION, from outside the agent: the file lists at its size with no
    # bytes on this device's disk.
    wait_until(
        lambda: host.residency(ondemand, name) is not None, window, interval=1.0,
        diagnose=lambda: (
            f"{name!r} never listed in the on-demand folder; the listing reads "
            f"{host.listing(ondemand)!r}\n{ondemand.diagnostics()}\n{other.diagnostics()}"
        ),
    )
    listed = host.listing(ondemand) or []
    assert name in listed, f"the folder's listing must name {name!r}: {listed!r}"
    seen = host.residency(ondemand, name)
    assert seen is not None and seen.size == len(body) and seen.cloud_only, (
        f"a file this device never opened must list as a placeholder — at its size "
        f"({len(body)}), its bytes on no disk of this device; read {seen!r}"
    )
    assert ondemand.sync_entry_states().get(name) == ("placeholder", False), (
        f"the engine's row for {name!r} must be a placeholder holding no local "
        f"identity; rows: {ondemand.sync_entry_states()!r}"
    )

    # Opening it (another process's read) downloads it and serves its bytes.
    read = host.read(ondemand, name)
    assert read.returncode == 0 and read.stdout == body, (
        f"another process's read through the on-demand root must return the other "
        f"device's bytes; rc={read.returncode} stdout={read.stdout!r} "
        f"stderr={read.stderr!r}\n{ondemand.diagnostics()}"
    )
    # The engine recorded the served file — the record that keeps a downloaded
    # file from reading as a local edit (`SyncEngine::mark_hydrated`) — and the
    # file is now held. Both polled: the record lands before the open completes,
    # and a cached attribute answer may still be the placeholder's for a moment.
    wait_until(
        lambda: ondemand.sync_entry_states().get(name) == ("synced", True),
        window, interval=0.5,
        diagnose=lambda: (
            f"the engine never recorded {name!r} as hydrated; rows: "
            f"{ondemand.sync_entry_states()!r}\n{ondemand.diagnostics()}"
        ),
    )
    wait_until(
        lambda: (host.residency(ondemand, name) or Residency(0, True, "")).cloud_only is False,
        window, interval=0.5,
        diagnose=lambda: (
            f"{name!r} still reads cloud-only after it was opened: "
            f"{host.residency(ondemand, name)!r}"
        ),
    )

    # MUTATION (UI): and back. The root comes down and the hydrated file is an
    # ordinary file in the user's directory.
    scope = open_bound_row(app, set_name, diagnose=ondemand.diagnostics)
    assert loc.toggle_mode(scope=scope) == "always", (
        f"the switch did not move the row back to always; error={app.error_text()!r}\n"
        f"{ondemand.diagnostics()}"
    )
    _await_root(host, ondemand, standing=False)
    wait_until(
        lambda: (ondemand.path / name).is_file()
        and (ondemand.path / name).read_bytes() == body,
        window, interval=1.0,
        diagnose=lambda: (
            f"{name!r} did not hold its bytes after the root came down; the directory "
            f"holds {sorted(p.name for p in ondemand.path.iterdir())!r}"
        ),
    )
    ordinary = host.residency(ondemand, name)
    assert ordinary is not None and not ordinary.cloud_only, (
        f"after the flip back {name!r} must be an ordinary file; read {ordinary!r}"
    )
    assert not app.has_error(), f"the round trip surfaced an error: {app.error_text()!r}"
