"""Comparing two snapshots — full-stack (tier_3) API E2E.

Witnesses `snapshots` outcome 14, *"Two snapshots can be compared to see
exactly what changed between them"* (``docs/goal/behavior/backup-restore.md``
§ Browsing Snapshots). ``fauna.filesync.snapshot.diff`` is served
(``bins/fauna-nest/src/filesync_handlers.rs`` ``diff_handler``) and had exactly
one caller in the tree — macOS's ``FaunaKit`` ``APIClient`` — which runs on a
machine the shared suite never drives, so no test anywhere called the kind.

**"Exactly what changed" is the assertion, not "a reply arrived."** All three
arms are exercised in one diff — a file added, a file removed, a file whose
bytes changed — because a handler that returned, say, only `added` would
satisfy any weaker shape while losing two thirds of the promise. Sizes are
distinct per file so each entry is identified without the nest's path-hash
derivation.

**Entries carry no plaintext path, by design.** Since the S9 path-sealing flip
``snapshot_files`` holds no plaintext column, so every diff entry ships
``path`` as the empty-string scrub sentinel plus the ``path_hash`` +
``path_sealed`` pair a keyed reader renders from
(``bins/fauna-nest/src/backup/diff.rs``). The test asserts that shape rather
than working around it: a diff that started leaking plaintext paths to the
wire would be a sealing regression, and this is the wire-level witness of it.

Process safety: no ``pkill``/``killall``; the nest is the session-scoped
``nest_instance`` and is torn down by its own fixture.
"""

from __future__ import annotations

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import (
    create_actor_and_register,
    create_folder_snapshot,
    sync_changes_record,
    sync_register,
    user_create_folder,
)

# One home for the second-boundary wait and its justification: `snapshots`
# carries `UNIQUE(folder_id, created_at)` at second granularity, so two
# snapshots of one folder genuinely need two distinct wall-clock seconds and a
# same-second repeat dedups to the first row rather than erroring.
from tests.api.test_snapshot_backups import _wait_for_next_wall_clock_second

pytestmark = pytest.mark.tier_3


def _ws(nest_instance, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _record(nest_instance, actor, device_id, folder, path, *, size, change_type="create"):
    """One `fauna.sync.changes.record` — the production kind the sync daemon
    drives. A fresh random `manifest_hash` per call is what makes a re-record
    of the same path a MODIFICATION rather than a no-op: `diff` compares
    manifest hashes, not sizes."""
    sync_changes_record(
        nest_instance["port"],
        secret_key=bytes(actor["signing_key"]).hex(),
        folder=folder,
        device_id=device_id,
        path=path,
        manifest_hash=secrets.token_bytes(32).hex(),
        size_bytes=size,
        change_type=change_type,
    )


@pytest.mark.feature("snapshots")
def test_diff_reports_added_removed_and_modified(nest_instance):
    """Snapshot A → change three things → snapshot B → the diff names all three.

    ``A`` holds ``keep`` (100 B) and ``gone`` (300 B). Between the two
    captures the owner's device adds ``fresh`` (250 B), rewrites ``keep``
    (→ 175 B, new manifest) and deletes ``gone`` — so ``B`` holds ``keep``
    (175 B) and ``fresh`` (250 B), and the diff must report exactly one entry
    in each of the three buckets, with the byte arithmetic to match.
    """
    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    secret = bytes(actor["signing_key"]).hex()
    device_id = secrets.token_hex(32)
    sync_register(nest_instance["port"], secret_key=secret, device_id=device_id)

    folder = f"snapdiff-{secrets.token_hex(4)}"
    user_create_folder(nest_instance["port"], folder, secret_key=secret)

    stem = secrets.token_hex(4)
    keep, gone, fresh = f"a/keep-{stem}.bin", f"a/gone-{stem}.bin", f"a/fresh-{stem}.bin"

    _record(nest_instance, actor, device_id, folder, keep, size=100)
    _record(nest_instance, actor, device_id, folder, gone, size=300)
    snap_a = create_folder_snapshot(
        nest_instance["port"], folder, secret_key=secret, device_id=device_id
    )
    assert snap_a["file_count"] == 2, (
        f"snapshot A must capture both seeded files, got {snap_a!r}"
    )

    _wait_for_next_wall_clock_second()
    _record(nest_instance, actor, device_id, folder, fresh, size=250)
    _record(nest_instance, actor, device_id, folder, keep, size=175, change_type="update")
    _record(nest_instance, actor, device_id, folder, gone, size=0, change_type="delete")
    snap_b = create_folder_snapshot(
        nest_instance["port"], folder, secret_key=secret, device_id=device_id
    )
    assert snap_b["id"] != snap_a["id"], (
        "the two captures dedupped to one row — they must land in distinct "
        f"wall-clock seconds: {snap_a!r} vs {snap_b!r}"
    )
    assert snap_b["file_count"] == 2, (
        f"snapshot B must hold `keep` + `fresh` and not the deleted `gone`, got {snap_b!r}"
    )

    with _ws(nest_instance, actor) as ws:
        diff = ws.call(
            "fauna.filesync.snapshot.diff", {"a": snap_a["id"], "b": snap_b["id"]}
        )

    assert diff["snapshot_a"] == snap_a["id"] and diff["snapshot_b"] == snap_b["id"], diff
    assert diff["folder"] == folder, diff

    added = diff["added"]
    removed = diff["removed"]
    modified = diff["modified"]
    assert [e["size_bytes"] for e in added] == [250], (
        f"the file added between the two captures must be the one entry in "
        f"`added` (250 B), got {added!r}"
    )
    assert [e["size_bytes"] for e in removed] == [300], (
        f"the file deleted between the two captures must be the one entry in "
        f"`removed` (300 B), got {removed!r}"
    )
    assert [(e["old_size"], e["new_size"]) for e in modified] == [(100, 175)], (
        f"the rewritten file must be reported as modified with both sizes, got "
        f"{modified!r}"
    )

    summary = diff["summary"]
    assert summary["added_count"] == 1, summary
    assert summary["removed_count"] == 1, summary
    assert summary["modified_count"] == 1, summary
    assert summary["added_bytes"] == 250, summary
    assert summary["removed_bytes"] == 300, summary
    assert summary["net_bytes"] == -50, summary

    # The sealed-label shape: addressable by hash + seal, never by plaintext.
    for entry in added + removed + modified:
        assert entry["path"] == "", (
            f"a diff entry must ship the scrub sentinel for `path`, not a "
            f"plaintext path: {entry!r}"
        )
        assert entry["path_hash"], f"a diff entry must carry its join key: {entry!r}"
        assert entry["path_sealed"], (
            f"a diff entry must carry the sealed label a keyed reader renders "
            f"from, else the row is unrenderable on every app: {entry!r}"
        )
