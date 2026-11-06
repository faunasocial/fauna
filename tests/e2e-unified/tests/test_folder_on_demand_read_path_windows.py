"""tier_3 — a windows on-demand folder is written through its cloud-files root.

The windows leg of `docs/features/files-on-demand.md` outcome 3 (*a file you
change inside an on-demand folder uploads like any other, and a file you put
there joins the folder*), the twin of
`test_folder_on_demand_read_path_linux.py` over the windows app's cloud-files
root (on-demand-files.md § Sync direction; apps/windows.md § the cloud-files
root is two-way). A fresh windows binding starts on-demand, so the journey is:

    bind (UI) → the row reads on-demand and the sync-root registration stands
      → another process (this test, standing in for Explorer) WRITES a new file
        into the root → the agent uploads it and the Media page lists it
      → the same process CHANGES that file in place → uploaded again
      → flip to always-resident (UI) → the registration is gone and the
        directory holds both files' bytes.

**Every assertion is another process's view**: the filter's sync-root
registration (`helpers.folder_content.on_demand_root_stands`), plain file I/O
through the root, and — for "uploaded" — the engine's own `file uploaded` line,
which it cannot emit without the byte plane having accepted the file. Bodies
are written as bytes: a text write turns each newline into CRLF on windows.

The windows app reaches the real agent only under BOTH real-agent markers
(conftest `_apply_isolated_sync_agent_env`); without them it binds into an
in-memory channel and runs no agent at all.
"""

import secrets
import time

import pytest

from helpers.folder_content import (
    SYNC_WINDOW_SECS,
    agent_diagnosis,
    await_agent_upload,
    bind_location_under_set,
    mountable_location,
    on_demand_root_stands,
    open_bound_row,
    upload_count,
)
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# A ceiling, not an expectation: the agent registers (or unregisters) the root
# after it has answered the flip, restarting the folder's engine in between.
_ROOT_WINDOW_SECS = 90.0


def _await_root(app, location, *, standing: bool) -> None:
    wait_until(
        lambda: on_demand_root_stands(location) == standing,
        _ROOT_WINDOW_SECS,
        interval=0.3,
        diagnose=lambda: (
            f"the on-demand root {'never stood over' if standing else 'never came down from'} "
            f"{location}; error={app.error_text()!r}\n" + agent_diagnosis(app, "owner")
        ),
    )


@pytest.mark.feature("files-on-demand")
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
def test_on_demand_folder_is_written_through_its_cloud_files_root(
    logged_in_app, request, tmp_path
):
    """A file put into, or changed inside, a windows on-demand folder uploads."""
    app = logged_in_app
    loc = app.sync_locations

    set_name = f"ondemand-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    folder = mountable_location(request, tmp_path, "on-demand-write")
    bind_location_under_set(app, set_name, folder, seat="owner")
    # The list may rebuild after the bind and shut the row; `open_bound_row` re-expands
    # it until its own switch paints and returns the row's current scope.
    scope = open_bound_row(app, set_name)

    # A fresh windows binding starts on-demand (on-demand-files.md § On-Demand
    # Files, *The choice is the user's*): no flip is owed, the root just stands.
    assert loc.mode_toggle_visible(scope=scope), (
        "the bound row must render folder-location-mode-toggle; "
        f"{app.driver.diagnose('folder-location-mode-toggle')} error={app.error_text()!r}"
    )
    assert loc.mode_toggle_state(scope=scope) == "on-demand", (
        f"a fresh windows binding starts on-demand; got {loc.mode_toggle_state(scope=scope)!r}"
    )
    _await_root(app, folder, standing=True)

    # A file put into the root by another process joins the folder.
    name = f"through-{secrets.token_hex(3)}.txt"
    body = f"written through the root {secrets.token_hex(8)}\n".encode()
    (folder / name).write_bytes(body)
    await_agent_upload(app, name, seat="owner")

    # …and the nest holds it: the Media page lists it.
    media = app.media

    def _listed_on_media() -> bool:
        media.reenter()
        return name in media.item_names()

    wait_until(
        _listed_on_media,
        60,
        interval=1.0,
        diagnose=lambda: f"the Media page never listed {name!r}; error={app.error_text()!r}",
    )

    # A CHANGE to a tracked file through the root uploads like any other (on
    # demand is a storage choice, never a direction choice). The engine has
    # already logged one upload of this path, so the witness is a NEW upload
    # line, counted.
    uploads_before = upload_count(app, name)
    changed = body + f"changed through the root {secrets.token_hex(8)}\n".encode()
    (folder / name).write_bytes(changed)
    deadline = time.monotonic() + SYNC_WINDOW_SECS
    while time.monotonic() < deadline and upload_count(app, name) <= uploads_before:
        time.sleep(2.0)  # sleep-ok: bounded poll cadence inside the deadline, not a settle guess
    assert upload_count(app, name) > uploads_before, (
        f"a change made to {name!r} through the on-demand root was never uploaded "
        f"within {SYNC_WINDOW_SECS:.0f}s; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )

    # MUTATION (UI): and back. The root comes down and the directory is an
    # ordinary directory holding the changed bytes.
    scope = open_bound_row(app, set_name)
    assert loc.toggle_mode(scope=scope) == "always", (
        f"the switch did not move the row to always; error={app.error_text()!r}\n"
        + agent_diagnosis(app, "owner")
    )
    _await_root(app, folder, standing=False)
    wait_until(
        lambda: (folder / name).exists() and (folder / name).read_bytes() == changed,
        SYNC_WINDOW_SECS,
        interval=1.0,
        diagnose=lambda: (
            f"{name!r} did not hold its changed bytes after the root came down; the directory "
            f"holds {sorted(p.name for p in folder.iterdir())!r}"
        ),
    )
    assert not app.has_error(), f"the round trip surfaced an error: {app.error_text()!r}"
