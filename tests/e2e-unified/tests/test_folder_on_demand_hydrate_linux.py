"""tier_3 — a file another device adds lists in an on-demand folder on a linux
host as a placeholder, and opening it downloads its bytes.

The placeholder half of the linux on-demand root, driven through the apps
(on-demand-files-linux.md § Linux FUSE binding — mount-over, and *hydrate on
open; write through*). `test_folder_on_demand_read_path_linux.py` proves the
switch mounts the root and that other processes read and write through it, but
every file it reads is one the device already held: one seat cannot produce a
file its own device lacks through the UI. Two seats of ONE account on one
machine can (e2e-live-sync-convergence.md — convention 16's shape):

    seat a (the linux app, or tui) — bound to the set, turned on-demand with the
      bound row's switch → the kernel's mount table shows `fuse.fauna`
    seat b (a tui seat) — drops a file into its own bound directory
      → on seat a, FROM SUBPROCESSES: `stat` lists the file at its size with NO
        blocks (a placeholder — the bytes are on no disk of this device), and
        the engine's row for it is a `placeholder` holding no local identity
      → `cat` returns seat b's bytes; afterwards the file has blocks and the
        engine's row is `synced` with the served file's identity recorded
      → flip back (UI) → the mount is gone and the directory is an ordinary
        directory holding the file.

The journey itself is shared with the windows twin
(`test_folder_on_demand_hydrate_windows.py`): `helpers.on_demand_hydrate` runs
it, and this module passes the FUSE probe (`FUSE_HOST`), which reads the mount
table and a child's `ls`/`stat`/`cat` through the mount.

**One journey, two tests, because a seat pair of two apps is not two witnesses.**
The app the outcome is observed on is seat a's. The linux test's tui seat only
supplies the file; its seat-set parameter names both apps, so the ledger writes
a tui record too, but the `linux` mark keeps it out of tui's cell
(`feature-catalog.md` § Cell semantics, the mark rule). tui's own test puts tui
on seat a, with a tui peer.

The seat trees sit under `/tmp` whatever `TMPDIR` says
(`helpers.folder_content.mountable_location`'s reason: Ubuntu's AppArmor
profile for `fusermount3` admits a mount point only under a short list).
"""

from __future__ import annotations

import pytest

from helpers.folder_content import require_fuse_host
from helpers.on_demand_hydrate import (
    FUSE_HOST,
    ROOT_WINDOW,
    hydrate_journey,
    seat_set_param,
)
from tests.test_filesync_seats import (
    STARTUP_WINDOW,
    WINDOW,
    _seat_run,
    _set_name_for,
)

pytestmark = pytest.mark.tier_3

LINUX_SEATS = ("native", "tui")
# Both seats tui: the second app under this test's lane is the lead app itself.
TUI_SEATS = ("tui", "tui")
_TIMEOUT = int(2 * STARTUP_WINDOW + 3 * WINDOW + 2 * ROOT_WINDOW + 300)


def _placeholder_journey(seat_set, nest_mode, tmp_path_factory, request) -> None:
    """The journey on seat a, whichever app sits there (module docstring)."""
    with _seat_run(
        seat_set, nest_mode, tmp_path_factory, request, seat_dir=FUSE_HOST.seat_dir
    ) as run:
        hydrate_journey(
            run,
            FUSE_HOST,
            seat_set=seat_set,
            set_name=_set_name_for(len(seat_set)),
            window=WINDOW,
        )


@pytest.mark.linux
@pytest.mark.parametrize("seat_set", [seat_set_param(LINUX_SEATS)])
@pytest.mark.timeout(_TIMEOUT)
@pytest.mark.feature("files-on-demand")
def test_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open(
    seat_set, nest_mode, tmp_path_factory, request
):
    """A file another device adds appears in the on-demand folder by itself,
    costs no disk until it is opened, and opening it serves its bytes."""
    _placeholder_journey(seat_set, nest_mode, tmp_path_factory, request)


@pytest.mark.tui
@pytest.mark.parametrize("seat_set", [seat_set_param(TUI_SEATS)])
@pytest.mark.timeout(_TIMEOUT)
@pytest.mark.feature("files-on-demand")
def test_tui_a_file_from_another_device_lists_as_a_placeholder_and_hydrates_on_open(
    seat_set, nest_mode, tmp_path_factory, request
):
    """The same promise with tui's bound row turning the folder on-demand."""
    require_fuse_host("tui")
    _placeholder_journey(seat_set, nest_mode, tmp_path_factory, request)
