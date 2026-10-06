"""Snapshot deletion safety — full-stack (tier_3) API E2E.

Witnesses `snapshots` outcome 12, *"A snapshot you delete is not gone at once:
it waits two days, then stays recoverable for a month before its data can be
released"* (``docs/goal/behavior/backup-restore.md`` § 7. Deletion Safety).
The existing API suite (``test_snapshot_backups.py``) drives ``list`` /
``prune`` / ``check`` only — it witnesses § 7's Layer 1 hard floor and the
prune dry run, and nothing in the tree ever called
``fauna.filesync.snapshot.delete`` or ``undelete`` over the wire. So the two
windows that make deletion *safe* — Layer 2's 48-hour cool-off and Layer 3's
30-day recovery month — were pinned nowhere an e2e could see.

**Latency-independent** (e2e-conventions.md convention 14). Neither window is
waited out. Layer 2 is asserted as the **recorded deadline** the delete reply
and the list row carry; the transition across it is driven by the production
executor through ``POST /api/v1/test/pending_actions/run_due``
(``bins/fauna-nest/src/pending_actions_test_hook.rs``), which zeroes
``execute_after`` and then calls the SAME
``pending_actions::execute_ready_actions`` the 60-second background tick calls
— so what is observed afterwards is the production apply path. Layer 3 is
asserted the same way, as the recorded ``purge_after`` the apply wrote.

**This test owns its nest.** ``run_due`` is deliberately *nest-wide*: it makes
**every** queued pending action due, so running it against the session-scoped
``nest_instance`` would fire other tests' handle changes and account deletions
out from under them.

Process safety: no ``pkill``/``killall``; the nest is started through the
framework's dedicated-nest seam and torn down by this fixture.
"""

from __future__ import annotations

import secrets
import time

import pytest
import requests

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register, create_folder_snapshot, user_create_folder

# One home for the second-boundary wait and its justification (see the import
# in `test_snapshot_diff.py`): `snapshots` carries `UNIQUE(folder_id,
# created_at)` at second granularity, so N distinct snapshots of one folder
# genuinely need N distinct wall-clock seconds.
from tests.api.test_snapshot_backups import _wait_for_next_wall_clock_second

pytestmark = pytest.mark.tier_3

#: `ActionType::SnapshotDelete.delay_secs()` — § 7 Layer 2's "waits two days".
EXECUTE_AFTER_SECS = 48 * 3600
#: `CacheDb::soft_delete_snapshot` — § 7 Layer 3's "recoverable for a month".
PURGE_AFTER_SECS = 30 * 24 * 3600
#: Slack on both deadline assertions: the nest stamps them from its own clock
#: at handler time, so the only thing being tolerated here is the round trip.
#: Wide enough never to flake on a loaded box, far narrower than any
#: neighbouring constant (6 h, 7 d, 14 d, 30 d) — a delay that regressed to
#: another `ActionType`'s would still red.
DEADLINE_SLACK_SECS = 900


@pytest.fixture()
def deletion_nest(request, nest_mode, tmp_path_factory):
    """A fresh nest, exclusive to this module (see the docstring re: the
    nest-wide reach of `run_due`)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "snapshot-deletion-nest"
    )
    yield nest
    cleanup()


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _row(nest, actor, folder, snapshot_id) -> dict:
    with _ws(nest, actor) as ws:
        rows = ws.call("fauna.filesync.snapshot.list", {"folder": folder})["rows"]
    match = [r for r in rows if r["id"] == snapshot_id]
    assert match, f"snapshot {snapshot_id} vanished from the folder listing: {rows!r}"
    return match[0]


def _run_due(nest) -> dict:
    """Make every queued pending action due and run the production executor
    once — the causal barrier across § 7 Layer 2."""
    resp = requests.post(
        f"{nest['url']}/api/v1/test/pending_actions/run_due", json={}, timeout=60
    )
    assert resp.status_code == 200, (
        f"test-hooks pending_actions run_due returned {resp.status_code}: {resp.text}"
    )
    return resp.json()


@pytest.mark.feature("snapshots")
def test_delete_waits_two_days_then_stays_recoverable_for_a_month(deletion_nest):
    """delete → 48 h pending → executor → soft-deleted with a 30-day window →
    undelete puts it back.

    Four snapshots, not one: § 7 Layer 1's hard floor (`SNAPSHOT_HARD_FLOOR =
    3`) refuses a delete that would take the set to fewer than three active
    snapshots, so a set at the floor cannot exercise Layers 2 and 3 at all —
    the delete would be refused before either window opened.
    """
    actor = create_actor_and_register(
        deletion_nest["port"], admin_signing_key=deletion_nest["admin"]["signing_key"]
    )
    secret = bytes(actor["signing_key"]).hex()
    folder = f"deletion-{secrets.token_hex(4)}"
    user_create_folder(deletion_nest["port"], folder, secret_key=secret)

    ids = []
    for i in range(4):
        if i > 0:
            _wait_for_next_wall_clock_second()
        ids.append(create_folder_snapshot(deletion_nest["port"], folder, secret_key=secret)["id"])
    assert len(set(ids)) == 4, f"the four captures must be distinct rows, got {ids!r}"
    target = ids[0]

    # ── Layer 2: the delete QUEUES, it does not remove ────────────────
    requested_at = int(time.time())
    with _ws(deletion_nest, actor) as ws:
        reply = ws.call("fauna.filesync.snapshot.delete", {"snapshot_id": target})
    assert reply["status"] == "pending", reply
    assert reply["pending_action_id"] > 0, reply
    delay = reply["execute_after"] - requested_at
    assert abs(delay - EXECUTE_AFTER_SECS) <= DEADLINE_SLACK_SECS, (
        f"a deleted snapshot must wait two days before anything happens to it: "
        f"execute_after is {delay}s out, want ~{EXECUTE_AFTER_SECS}s ({reply!r})"
    )

    row = _row(deletion_nest, actor, folder, target)
    assert row["deletion_pending"] is True, (
        f"the row must say a deletion is in flight for it, got {row!r}"
    )
    assert row["soft_deleted"] is False, (
        f"nothing may be deleted yet — the two days have not passed: {row!r}"
    )
    # `.get`, not `[...]`: `purge_after` / `execute_after` are
    # `skip_serializing_if = "Option::is_none"` on `SnapshotSummaryRow`, so a
    # `None` is an ABSENT key on the wire, never a null.
    assert row.get("purge_after") is None, row
    assert row["execute_after"] == reply["execute_after"], (
        "the list must surface the SAME deadline the delete reply promised, "
        f"else no app can render the countdown: {row!r} vs {reply!r}"
    )

    # ── Layer 3: the executor fires → a month of recoverability ───────
    ran = _run_due(deletion_nest)
    assert ran.get("executed", 0) >= 1, (
        f"the queued snapshot delete never executed: {ran!r}"
    )
    executed_at = int(time.time())

    row = _row(deletion_nest, actor, folder, target)
    assert row["soft_deleted"] is True, (
        f"after the two days the snapshot must be deleted — softly: {row!r}"
    )
    assert row["deletion_pending"] is False, row
    assert row.get("execute_after") is None, (
        "a fired action's window is closed; the row's remaining window is "
        f"`purge_after`: {row!r}"
    )
    window = row["purge_after"] - executed_at
    assert abs(window - PURGE_AFTER_SECS) <= DEADLINE_SLACK_SECS, (
        f"a soft-deleted snapshot must stay recoverable for a month before its "
        f"data can be released: purge_after is {window}s out, want "
        f"~{PURGE_AFTER_SECS}s ({row!r})"
    )

    # ── The month is real recoverability, not a countdown ─────────────
    with _ws(deletion_nest, actor) as ws:
        undelete = ws.call("fauna.filesync.snapshot.undelete", {"snapshot_id": target})
    assert undelete["undeleted"] is True, undelete

    row = _row(deletion_nest, actor, folder, target)
    assert row["soft_deleted"] is False, (
        f"undelete inside the recovery month must bring the snapshot back: {row!r}"
    )
    assert row.get("purge_after") is None and row["deletion_pending"] is False, row

    # And it is back for real: a second undelete has nothing to undo, which is
    # the nest agreeing the row is active again rather than merely re-flagged.
    with _ws(deletion_nest, actor) as ws:
        with pytest.raises(RpcCallError) as excinfo:
            ws.call("fauna.filesync.snapshot.undelete", {"snapshot_id": target})
    assert "not" in str(excinfo.value).lower(), excinfo.value
