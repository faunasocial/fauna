"""A device holding a file only as an on-demand placeholder never downloads it
just to hand it to another of your devices.

Owner: ``docs/goal/behavior/relay-serving.md`` § Relay serving → *Which seats
hold*: an on-demand root the agent hosts serves its hydrated bodies, and "a
placeholder answers none" — a seat never hydrates on an asker's behalf. The
same-account peer plane serves through the same engine (``sibling_first`` asks
every admitted sibling before the nest; ``fauna_sync_engine::sibling_chunks``),
so a placeholder's answer to a sibling is the same none.

**What this pins that the tier below cannot.** The serve core's refusal is
pinned one tier down (``relay_serve_test``'s
``an_on_demand_replica_serves_a_body_it_hydrated_and_declines_a_placeholder``).
What it cannot answer is whether, between real seats — each a real app whose
agent mounts a real on-demand root — the device asked for a body it lists only
as a placeholder declines rather than downloading it to pass it on.

**The journey.** Three seats of one account on one machine, no operator
(convention 16), the folder at full residency so the nest holds the bytes:

    seat a — the folder bound ON DEMAND (the bound row's switch, convention 8)
    seat c — the asker, also on demand, so it fetches only when a process opens
      the file: the ask happens after a provably holds the placeholder, never
      in a race with a's own listing of it
    seat b — writes the file, waits for its upload, and goes away, so a is the
      only sibling left that could answer

then another process opens the file on c. Three observables, none of them time:

1. c got the bytes, byte for byte;
2. c's agent asked a sibling and was served nothing — its ``peer_files``
   reading (``GetServiceStatus``) counts no chunk from a sibling and at least
   one chunk left to the nest, which it counts only when it held a sibling to
   ask (``SiblingChannels::fetch``) — and a was the one sibling it held;
3. on a the file is still a placeholder: the engine's own row is
   ``placeholder`` with no local identity (``SyncEngine::mark_hydrated`` records
   one on any hydration), and on a linux host ``stat`` through the mount still
   reports no blocks.

**Which seats.** The trios of :data:`sync_seats.DEFAULT_TRIOS` wherever the
agent hosts an on-demand root behind the per-binding switch: tui and the linux
app on a linux host (the FUSE root), the windows app on a windows host (cfapi).
macOS is marked and declared unbuilt (the ``on-demand-replica-serve-seat``
surface): a bound location there is always-resident by design and apple's
on-demand replica is the File Provider extension, another process that serves
no sibling yet (relay-serving.md § Which seats hold). A macOS trio held through
the agent alone would pass vacuously — a's agent runs no engine for a set it
shows only through the extension, so its "nothing" says nothing about a
placeholder. The leg lands with the extension's seat, through the set-level
``folder-on-demand-toggle``. The phones are excused on the catalog page.

On linux the seat trees sit under ``/tmp`` whatever ``TMPDIR`` says
(``helpers.folder_content.mountable_location``'s reason: the confined
``fusermount3`` admits a mount point only under a short list).
"""

import contextlib
import secrets
import shutil
import sys
import tempfile
from pathlib import Path

import pytest

from common import user_create_folder
from helpers import sync_seats
from helpers.folder_content import _log_path, atomic_write, on_demand_mount_stands
from helpers.fuse_mount import release_mount, run_through_mount
from helpers.waiting import wait_until
from tests.test_device_to_device_file_transfer import (
    BODY_BYTES,
    SIBLING_WINDOW,
    STARTUP_WINDOW,
    WINDOW,
    _account,
    _reading,
    _seat_factory,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui led; the linux and windows apps run their native trios through the
    # same seat seam. macOS and the phones: module docstring.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.requires_surface("on-demand-replica-serve-seat"),
]

_ON_LINUX = sys.platform.startswith("linux")

# This journey's own owed legs, in `sync_seats.UNBUILT_MODES`' shape. The
# `requires_surface` marker answers by APP alone, and the tui is a serving seat
# where it hosts the agent's on-demand root (linux FUSE, windows cfapi) — but a
# tui on a macOS host binds always-resident (`apps/fauna-tui/src/sync_agent.rs`,
# `BindingView::mode` is `None` there), so its trio has no switch to flip and
# would fail at the bound row rather than say anything about a placeholder. It
# lands with the same File Provider seat as the macos arm (module docstring).
OWED_SEATS: dict = {
    "darwin": {
        "tui": "a tui on a macOS host binds always-resident: apple's on-demand "
        "replica is the File Provider extension, which serves no sibling yet "
        "(surfaces.toml `on-demand-replica-serve-seat`)",
    },
}

_SEAT_TRIOS = [
    pytest.param(trio, id=sync_seats.seat_set_param_id(trio))
    for trio in sync_seats.seat_sets_for_platform(
        sys.platform, sync_seats.DEFAULT_TRIOS, unbuilt=OWED_SEATS
    )
]

#: The agent mounts (or unmounts) after it has answered the flip, restarting
#: the folder's engine.
MOUNT_WINDOW = 90.0


def _go_on_demand(seat, folder: str) -> None:
    """Turn ``seat``'s bound row on demand through the row's own switch — a
    no-op where a fresh binding already starts there (a windows host) — and,
    on a linux host, wait for the FUSE root over the bound directory."""
    app = seat.app
    b = app.backups
    b.navigate_folders()
    scope = f"folder-row[{b.open_folder(folder)}]"
    loc = app.sync_locations
    assert loc.mode_toggle_visible(scope=scope), (
        f"seat {seat.name}: the bound row must render folder-location-mode-toggle; "
        f"{app.driver.diagnose('folder-location-mode-toggle')} error={app.error_text()!r}"
    )
    if loc.mode_toggle_state(scope=scope) != "on-demand":
        assert loc.toggle_mode(scope=scope) == "on-demand", (
            f"seat {seat.name}: the switch did not move the row on demand; "
            f"error={app.error_text()!r}\n{seat.diagnostics()}"
        )
    if _ON_LINUX:
        wait_until(
            lambda: on_demand_mount_stands(seat.path), MOUNT_WINDOW, interval=0.3,
            diagnose=lambda: (
                f"seat {seat.name}: the on-demand root was never mounted over "
                f"{seat.path}; error={app.error_text()!r}\n{seat.diagnostics()}"
            ),
        )


def _await_placeholder(seat, name: str, *others) -> None:
    wait_until(
        lambda: seat.sync_entry_states().get(name) == ("placeholder", False),
        WINDOW, interval=1.0,
        diagnose=lambda: (
            f"seat {seat.name} never listed {name!r} as a placeholder holding no "
            f"local identity; rows: {seat.sync_entry_states()!r}\n"
            f"--- seat {seat.name} ---\n{seat.diagnostics()}\n"
            + "".join(f"--- seat {o.name} ---\n{o.diagnostics()}\n" for o in others)
        ),
    )


def _open(seat, name: str) -> bytes:
    """Another process opens the file on ``seat`` — the open that hydrates it.
    On linux a bounded child ``cat`` through the mount (a hung FUSE request
    fails naming the seat's state, never wedges the run)."""
    if _ON_LINUX:
        out = run_through_mount(seat.path, "cat", str(seat.path / name), diagnose=seat.diagnostics)
        assert out.returncode == 0, (
            f"`cat` through seat {seat.name}'s mount failed: rc={out.returncode} "
            f"stderr={out.stderr!r}\n{seat.diagnostics()}"
        )
        return out.stdout
    return (seat.path / name).read_bytes()


def _blocks(seat, name: str) -> int | None:
    out = run_through_mount(seat.path, "stat", "-c", "%b", str(seat.path / name),
                            diagnose=seat.diagnostics)
    return int(out.stdout.decode().strip()) if out.returncode == 0 else None


@pytest.mark.timeout(int(3 * STARTUP_WINDOW + 3 * MOUNT_WINDOW + SIBLING_WINDOW + 4 * WINDOW + 180))
@pytest.mark.parametrize("seat_trio", _SEAT_TRIOS)
@pytest.mark.feature("device-to-device-file-transfer")
def test_a_placeholder_is_never_downloaded_to_hand_to_another_device(
    nest_instance, request, seat_trio
):
    """Seat ``a`` lists a file only as a placeholder; when seat ``c`` opens it,
    with ``a`` the one sibling it can ask, ``a`` serves nothing, the bytes come
    from the nest, and ``a``'s copy is still a placeholder."""
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"placeholder-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    run_token = sync_seats.new_run_token(seats=3)
    name = f"{run_token}-placeholder.bin"
    body = secrets.token_bytes(BODY_BYTES)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-", dir="/tmp" if _ON_LINUX else None))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_trio)
    logged = _log_path(name)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat("a"))
            b = stack.enter_context(seat("b"))
            c = stack.enter_context(seat("c"))
            for s in (a, b, c):
                s.await_ready(STARTUP_WINDOW)
            _go_on_demand(a, folder)
            _go_on_demand(c, folder)

            # b writes the file and waits until the nest holds it.
            atomic_write(b.path / name, body)
            wait_until(
                lambda: any(
                    "file uploaded" in line and logged in line for line in b.agent_log_lines()
                ),
                WINDOW, interval=1.0,
                diagnose=lambda: f"seat b never uploaded {name!r}\n{b.diagnostics()}",
            )
            # Both on-demand seats list it as a placeholder BEFORE anyone asks.
            _await_placeholder(a, name, b, c)
            _await_placeholder(c, name, a, b)

            # b goes away: a is the one sibling c can ask.
            b.stop()
            wait_until(
                lambda: _reading(c).get("siblings", 0) == 1 and _reading(a).get("siblings", 0) == 1,
                SIBLING_WINDOW, interval=2.0,
                diagnose=lambda: (
                    f"seats a and c never settled on each other as their one sibling "
                    f"(a {a.peer_files()!r}, c {c.peer_files()!r}) — without it the "
                    f"ask below would prove nothing about a\n"
                    f"--- seat a ---\n{a.diagnostics()}\n--- seat c ---\n{c.diagnostics()}"
                ),
            )
            before = _reading(c)

            # (1) another process opens the file on c, and gets the bytes.
            got = _open(c, name)
            assert got == body, (
                f"opening {name!r} on seat c returned {len(got)} bytes, not the "
                f"{len(body)} seat b wrote\n{c.diagnostics()}"
            )

            # (2) c asked its sibling and was served nothing: the nest served it.
            after = _reading(c)
            served = after.get("chunks", 0) - before.get("chunks", 0)
            left = after.get("to_nest", 0) - before.get("to_nest", 0)
            assert served == 0 and left >= 1, (
                f"seat c's agent reading went {before!r} -> {after!r} ({served} "
                f"chunk(s) served by a sibling, {left} left to the nest): "
                + (
                    "seat a, holding the file only as a placeholder, served it"
                    if served else
                    "seat c never asked a sibling, so nothing here shows a declining"
                )
                + f"\n--- seat c ---\n{c.diagnostics()}\n--- seat a ---\n{a.diagnostics()}"
            )

            # (3) a never hydrated it on c's behalf.
            assert a.sync_entry_states().get(name) == ("placeholder", False), (
                f"seat a's row for {name!r} is no longer a placeholder holding no "
                f"local identity after seat c asked for it: "
                f"{a.sync_entry_states().get(name)!r}\n{a.diagnostics()}"
            )
            if _ON_LINUX:
                assert _blocks(a, name) == 0, (
                    f"{name!r} reports {_blocks(a, name)!r} blocks through seat a's "
                    f"mount after seat c asked for it — the bytes landed on a's disk"
                )
    finally:
        # The seats are stopped by now, so a mount still standing is a dead
        # daemon's: bring it down first. Never delete through a standing view —
        # that is a user delete of every file in the set.
        if not _ON_LINUX or all(release_mount(tmp / s / "watch") for s in ("a", "c")):
            shutil.rmtree(tmp, ignore_errors=True)
