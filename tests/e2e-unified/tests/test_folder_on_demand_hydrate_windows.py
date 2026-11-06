"""tier_3 — a file another device adds lists in a windows on-demand folder as a
cloud-only placeholder, and opening it downloads its bytes.

The windows leg of `docs/features/files-on-demand.md` outcomes 11 and 12, the
twin of `test_folder_on_demand_hydrate_linux.py` over the windows app's
cloud-files root (on-demand-files.md § On-Demand Files — hydrate on open is the
shared mechanism; the binding is apps/windows.md § Shell Extension).
`test_folder_on_demand_read_path_windows.py` proves another process writes
through the root, but one seat cannot produce a file its own device lacks
through the UI. Two seats of ONE account on one machine can
(e2e-live-sync-convergence.md — convention 16's shape):

    seat a (the windows app) — bound to the set; a fresh windows binding starts
      on-demand, so no flip is owed: the row's switch reads on-demand and the
      filter answers the directory is a sync root
    seat b (a tui seat) — drops a file into its own bound directory
      → on seat a, from OUTSIDE the agent: the directory lists the file at its
        size and the OS marks it a placeholder (`FILE_ATTRIBUTE_OFFLINE` /
        `RECALL_ON_DATA_ACCESS`), and the engine's row for it is a
        `placeholder` holding no local identity
      → a child `cmd /c type` returns seat b's bytes; afterwards the placeholder
        bits are gone and the engine's row is `synced` with the served file's
        identity recorded
      → flip to always-resident (UI) → the registration is gone and the
        directory holds an ordinary file with those bytes.

The journey is `helpers.on_demand_hydrate`'s, shared with linux; this module
passes the cloud-files probe (`CLOUD_FILES_HOST`). Every verdict is another
process's view: the agent is the provider, and cfapi fires no fetch for the
provider's own I/O, so the test process and its children are exactly where a
user's Explorer stands (on-demand-files.md § A placeholder is
present-but-unreadable).

The tui seat only supplies the file. The seat-set parameter names both apps, so
the ledger writes a tui record too, but the module's `windows` mark keeps it out
of tui's cell (`feature-catalog.md` § Cell semantics, the mark rule): this is a
witness for windows' column. Each windows seat runs an agent of its own on its
own pipe and data dir (`helpers.sync_seats.isolate_windows_sync_agent`), so the
real-agent markers are not needed here.
"""

from __future__ import annotations

import pytest

from helpers.on_demand_hydrate import (
    CLOUD_FILES_HOST,
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

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

WINDOWS_SEATS = ("native", "tui")
_TIMEOUT = int(2 * STARTUP_WINDOW + 3 * WINDOW + 2 * ROOT_WINDOW + 300)


@pytest.mark.parametrize("seat_set", [seat_set_param(WINDOWS_SEATS)])
@pytest.mark.timeout(_TIMEOUT)
@pytest.mark.feature("files-on-demand")
def test_a_file_from_another_device_lists_as_a_cloud_only_placeholder_and_hydrates_on_open(
    seat_set, nest_mode, tmp_path_factory, request
):
    """A file another device adds appears in the windows on-demand folder by
    itself, costs no disk until it is opened, and opening it serves its bytes."""
    with _seat_run(
        seat_set,
        nest_mode,
        tmp_path_factory,
        request,
        seat_dir=CLOUD_FILES_HOST.seat_dir,
    ) as run:
        hydrate_journey(
            run,
            CLOUD_FILES_HOST,
            seat_set=seat_set,
            set_name=_set_name_for(len(seat_set)),
            window=WINDOW,
        )
