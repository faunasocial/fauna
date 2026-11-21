"""The `back-up-and-restore` main use case, walked by gestures alone
(`docs/goal/behavior/main-use-cases.md` § The journeys; the steps are
catalogued in `use-cases.yaml`).

A person makes a folder, puts a file in it, backs it up, and gets the file back
from the backup. The journey starts where a user is after signing in, on the
feed, and goes:

1. Settings → Folders by its tab and rail row, the folder wizard (name, next,
   next, create), the new folder's row, and the row's binding form — a local
   directory bound through `folder-location-path-input` and
   `folder-location-add-button` (`ui/folders.md` § Layout & flow). A
   wizard-made folder is empty; the binding is how a person's own files enter
   it.
2. The person drops a file with known bytes into the bound directory. That is
   not an app gesture, it is the person's file system; the real sync agent
   uploads it, and the test waits on the agent's own `file uploaded` log line
   (`helpers/folder_content.await_agent_upload`) — an observation, never a
   write.
3. Backups by its tab, the folder picked on `backup-folder-selector`, a
   snapshot taken with `snapshot-create-button` (deterministic, where the
   scheduler's quiet period is not — `backup-restore.md` § 1), the snapshot
   opened from its `snapshot-item` row, and the file restored with its
   `snapshot-file-download-button` (single-file restore, `backup-restore.md`
   § 3). The `restore-confirm-*` controls are the mail/calendar kind restore
   and are not this journey.

The outcome where a person sees it: the restored file is on disk with exactly
the bytes they put in the folder. Under e2e the native save dialog is bypassed
and the app writes into `driver.download_dir()` (`BackupsActions.
wait_for_downloaded_file`).

tier_3: a real nest, a real app and a real sync agent, nothing stubbed. tui is
the lead app; linux, windows and macOS bind a local folder too and join through
the background tester. iOS and android bind no local folder through this form
(the `folder-location-*` ids are scoped to the four desktop-class apps and tui
in ui.yaml), and web has no local file system to bind.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.folder_content import atomic_write, await_agent_upload
from helpers.nav_tab_click import click_nav_tab
from helpers.use_case_walk import assert_use_case_walked

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    # The bound folder's file must reach the nest through the real agent: the
    # snapshot is cut from the head feed the agent's upload writes.
    pytest.mark.real_sync_agent,
    pytest.mark.isolated_sync_agent,
]


def _newest_snapshot_row(app) -> int:
    """The index of the row carrying the highest snapshot id — the one the
    create just cut. The scheduler may cut its own after the upload too; either
    holds the file, and the newest is never one from before it."""
    b = app.backups
    ids = [b.snapshot_row_id(i) for i in range(b.snapshot_count())]
    assert ids, f"no snapshot rows to open: {app.driver.diagnose('snapshot-item')}"
    return max(range(len(ids)), key=ids.__getitem__)


@pytest.mark.use_case("back-up-and-restore")
@pytest.mark.feature("snapshots")
# Documented-long (convention 9): one app, one real sync agent, a bind, an
# upload and a restore.
@pytest.mark.timeout(900)
def test_a_person_backs_up_a_folder_and_gets_a_file_back(logged_in_app, tmp_path):
    app = logged_in_app
    b = app.backups
    name = f"keep-{secrets.token_hex(4)}"
    filename = f"letter-{secrets.token_hex(3)}.txt"
    data = f"the words I must not lose {secrets.token_hex(16)}\n".encode()
    location = tmp_path / "my-folder"
    location.mkdir()

    # The use case starts on the feed (use-cases.yaml).
    app.feed.navigate()

    # ── 1. A folder of my own, bound to a directory on this device. ──
    b.navigate_folders(by_click=True)
    b.create_folder_via_wizard(name)
    b.bind_location_here(name, str(location))

    # ── 2. I put a file in it; the agent uploads it. ──
    atomic_write(location / filename, data)
    await_agent_upload(app, filename, seat="owner")

    # ── 3. Back it up, and get the file back. ──
    click_nav_tab(app.driver, "backups-tab", canary="backup-folder-selector")
    b.select_folder(name)
    b.create_snapshot()
    b.open_snapshot(index=_newest_snapshot_row(app))
    assert b.wait_for_snapshot_files(min_count=1) >= 1, (
        f"the snapshot of {name!r} lists no file to restore: "
        f"{app.driver.diagnose('snapshot-file-download-button')} "
        f"error={app.error_text()!r}"
    )
    restored = b.download_file_and_read(0, filename)

    # The outcome where the person sees it: the file is back, byte for byte.
    assert restored == data, (
        f"the restored {filename!r} differs from the file put in the folder: "
        f"{len(restored)} vs {len(data)} bytes, first-16 "
        f"{restored[:16].hex()} vs {data[:16].hex()}"
    )
    assert_use_case_walked(app.driver, "back-up-and-restore")
