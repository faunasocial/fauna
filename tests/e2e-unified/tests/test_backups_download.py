"""Single-file restore through the Backups page UI (tier_3).

The user journey behind `snapshot-file-download-button` (ratified 2026-07-12;
`docs/goal/ui/backups.md` § Layout & flow, `docs/goal/behavior/backup-restore.md`
§ 3): open Backups → select the folder → open a snapshot → click the per-file
download button → the client fetches the file's bytes through the shared
client-side walk (`fauna_core::file_download`; on windows via the FFI free fn
`download_snapshot_file_bytes`) and saves them. Under e2e the platform-native save dialog is
bypassed (not e2e-driveable) and the client writes into
`driver.download_dir()`; the test asserts the saved bytes are identical to what
was backed up.

The API side — `fauna.filesync.snapshot.get` carrying the file listing — is
pinned by `tests/api/test_snapshot_backups.py::test_snapshot_get_lists_the_seeded_file`;
a failure here but not there is UI-side.

Seeding uses `seed_snapshot_with_file_bytes` — real chunk + manifest blobs, so
the snapshot's file is genuinely byte-servable (an empty-set seed has no
manifest to fetch).

Process safety: no ``pkill``/``killall``; nest + app lifecycles belong to their
session fixtures.
"""

from __future__ import annotations

import secrets

import pytest

from common.auth import seed_snapshot_with_file_bytes, user_create_folder

# windows led the NATIVE side of the download affordance (web led the shared
# walk); linux is the first no-FFI native leg (Rust-native — no UniFFI hop).
# apple (macos+ios) landed 2026-07-22 — pure consume of the FFI free fn
# windows led, saved via the new SnapshotFileSaver e2e-bypass seam. Extend
# further as the android lift lands (ui/backups.md § Implementation status
# today → "Per-file download").
#
# ⚠ `tui` was missing from this list until 2026-08-21, and NOT for a reason —
# the set simply stopped growing. tui had painted `snapshot-file-download-button`
# over the same shared walk since before the parity milestone
# (`apps/fauna-tui/src/backups.rs`, and `ui-actual-tui.yaml` calls the snapshot
# family COMPLETE), and `drivers/tui.py::download_dir` already mirrored
# `drivers/linux.py`'s — so every piece this test needs was in place while the
# marker set kept it deselected on the DEFAULT app set. That is the costly
# direction of this mistake: the one app the bare `pytest` run exercises was the
# one app never exercising this journey. Contrast `test_folder_paywall.py`, whose
# tui absence is genuinely decided, stated, and given a re-add condition — an
# exclusion should always read like that one, never like this one did.
pytestmark = [
    pytest.mark.tier_3, pytest.mark.windows, pytest.mark.linux,
    pytest.mark.macos, pytest.mark.ios, pytest.mark.tui,
]


@pytest.mark.feature("snapshots")
def test_download_single_file_bytes_roundtrip(logged_in_app, nest_instance, test_user):
    """Click `snapshot-file-download-button` on a seeded snapshot file and
    assert the saved bytes match the seeded bytes exactly."""
    data = secrets.token_bytes(4096)
    filename = f"restore-me-{secrets.token_hex(4)}.bin"
    path = f"docs/{filename}"
    fs = f"dl-{secrets.token_hex(4)}"
    secret_hex = test_user["signing_key"].encode().hex()

    # mode defaults to "sync". (Historical note: this seed HAD to be
    # sync-type until 2026-07-17 — custody-copy records then routed into the
    # `backup_custody` projection, which the snapshot subsystem didn't read,
    # so a backup-type seed yielded an EMPTY snapshot no matter how correct
    # the UI was. Since the 2026-08-17 head unification every ordinary mode
    # records into `sync_changes` and snapshots from it
    # (`backup-restore.md` § Backup folders and snapshots; tier_3 pin
    # `bins/fauna-nest/tests/backup_mode_snapshot_coverage.rs`), so either
    # mode works here; sync-type stays as the simplest seed.)
    user_create_folder(nest_instance["port"], fs, secret_key=secret_hex)
    seeded = seed_snapshot_with_file_bytes(
        nest_instance["port"],
        secret_key=secret_hex,
        folder=fs,
        path=path,
        data=data,
    )

    app = logged_in_app
    app.backups.navigate()
    app.backups.select_folder(fs)
    # Wait for THIS seeded snapshot by id, not for "some row exists": the newly
    # selected set renders behind the previously selected set's still-mounted
    # rows, so a bare count opens a different set's snapshot. Across a combined
    # `--app macos,ios` run that also crosses parametrizations — `[ios]` opened
    # `[macos]`'s snapshot and saved ITS `restore-me-*.bin`, which looks like a
    # reused download directory and is not. See `wait_for_snapshot_row`.
    row = app.backups.wait_for_snapshot_row(seeded["snapshot_id"])
    app.backups.open_snapshot(index=row)

    # A bare count IS sufficient here: exactly ONE snapshot is opened after a
    # fresh `navigate()`, so there are no earlier file rows mounted that could
    # satisfy the threshold — the stale-collection hazard needs a *previous* open.
    assert app.backups.wait_for_snapshot_files(min_count=1) >= 1, (
        "the opened snapshot detail should list the seeded file with a "
        "per-file download button: "
        f"{app.driver.diagnose('snapshot-file-download-button')} "
        f"error={app.error_text()!r}"
    )

    app.backups.download_file(index=0)
    saved = app.backups.wait_for_downloaded_file(filename)
    assert saved == data, (
        f"saved bytes differ from the backed-up bytes: {len(saved)} vs "
        f"{len(data)} bytes, first-16 {saved[:16].hex()} vs {data[:16].hex()}"
    )
