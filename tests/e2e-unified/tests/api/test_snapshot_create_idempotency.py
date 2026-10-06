"""Same-second `snapshot.create_message_kind` repeats dedup — full-stack (tier_3).

The regression pin for a bug reported on 2026-07-24 while the tui Backups
restore half was being landed: `test_backups_restore.py` failed its third case
at a *setup* WS-RPC, before any UI touch, while each case passed in isolation.

**The bug.** `snapshots` carries `UNIQUE(folder_id, created_at)`
(`bins/fauna-nest/src/db/migrations.rs`) and `created_at` is wall-clock
*seconds*. Two `fauna.filesync.snapshot.create_message_kind` calls for the same
actor+kind inside one second therefore collided, and
`db/snapshots.rs::create_message_kind_snapshot_row` let the `ConstraintViolation`
escape as a client-visible `fauna.filesync.snapshot.internal`. The *folder*
create path (`db/sync_storage.rs::create_snapshot`) had already decided this
must not happen and dedups to the existing row; the message-kind path had not.
That is drift (priority #4), and `backup-restore.md` § 1 + § 5 already ruled for
dedup — see § 5's "Same-second repeats dedup, keeping the fresher capture".

**Why it matters beyond the harness.** A user clicking "create snapshot" twice
in quick succession — or any client retrying a create inside one second — got an
internal error from a plain, legitimate action, which is the "no client-causable
error state" invariant (`nest/common.md`). It also made `test_backups_restore.py`
timing-dependent for *every* client: a full-module run could fail on whichever
client's UI happened to be fast, while each case passed solo.

**Latency-independent by construction (convention 14).** Neither test asserts a
duration or sleeps to provoke the race. `test_repeated_create_cycles` issues its
creates back to back, which lands them inside one second on any machine at any
load — and if the box is so slow that they straddle a second boundary, the calls
simply all succeed with distinct ids, which is also a pass. There is no window
in which a green run depends on the machine being quiet.

Each test gets its own nest (the `snap_nest` fixture): the reserved `__mail`
folder name is globally UNIQUE in nest's schema, so only one actor per nest
can own a `__mail` snapshot (same constraint `test_dr_restore.py` documents).

Process safety: no `pkill`/`killall`; the nest is started via the framework's
`start_nest` helper and only this test's own process handle is killed.
"""

from __future__ import annotations

import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def snap_nest(request, nest_mode, tmp_path_factory):
    """A fresh nest, exclusive to one test (see module docstring re: `__mail`)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "snap-nest")
    yield nest
    cleanup()


def _ws_client(nest: dict, actor: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def test_same_second_repeat_creates_dedup_to_one_snapshot(snap_nest):
    """Two creates landing in one wall-clock second dedup to one row.

    **Non-vacuous by construction.** A pair of creates only exercises the bug
    if both land in the same second, so this test does not *assume* they did —
    it reads each snapshot's `created_at` back and keeps trying pairs until it
    observes a genuine same-second pair. It can only pass by having actually
    seen one, so a machine too slow to collide fails loudly rather than going
    green having tested nothing.

    **Latency-independent** (convention 14): the retry runs against a generous
    named deadline with no timing assertion anywhere. On any sane machine the
    first pair collides and the loop costs one iteration; the budget exists
    only so a pathologically slow box reports honestly instead of flaking.
    """
    owner = create_actor_and_register(
        snap_nest["port"], admin_signing_key=snap_nest["admin"]["signing_key"]
    )

    # Generous: one iteration is the expected cost. Sized far above any
    # non-pathological delay, never tuned to observed timings.
    SAME_SECOND_PAIR_BUDGET_S = 120.0

    with _ws_client(snap_nest, owner) as ws:

        def _create() -> int:
            reply = ws.call(
                "fauna.filesync.snapshot.create_message_kind",
                {"kind": "mail"},
            )
            # The regression: this raised RpcCallError
            # `fauna.filesync.snapshot.internal` on the second same-second call.
            assert reply["kind"] == "mail"
            snapshot_id = reply["snapshot_id"]
            assert isinstance(snapshot_id, int) and snapshot_id > 0, (
                f"create returned a bad snapshot_id: {reply!r}"
            )
            return snapshot_id

        def _created_at() -> dict[int, int]:
            listed = ws.call(
                "fauna.filesync.snapshot.list",
                {"message_kind": "mail", "limit": 0},
            )
            return {row["id"]: row["created_at"] for row in listed["rows"]}

        deadline = time.monotonic() + SAME_SECOND_PAIR_BUDGET_S
        observed_same_second = False
        attempts = 0

        while time.monotonic() < deadline and not observed_same_second:
            attempts += 1
            first, second = _create(), _create()
            rows = _created_at()

            # Every returned id must be a real, listable snapshot — the dedup
            # path must return the *existing* row, never a fabricated id.
            for sid in (first, second):
                assert sid in rows, (
                    f"create returned id {sid}, which snapshot.list does not "
                    f"report: {sorted(rows)}"
                )

            if first == second:
                # Deduped — which by construction means they shared a second.
                observed_same_second = True
            elif rows[first] == rows[second]:
                pytest.fail(
                    f"two creates at created_at={rows[first]} (the same wall-clock "
                    f"second) returned different snapshot ids {first} and {second} — "
                    "the UNIQUE(folder_id, created_at) dedup did not happen"
                )
            # else: the pair straddled a second boundary — not the case under
            # test. Try another pair.

        assert observed_same_second, (
            f"could not land two creates in the same wall-clock second within "
            f"{SAME_SECOND_PAIR_BUDGET_S}s ({attempts} pairs attempted) — the "
            "same-second path was never exercised, so this test proved nothing"
        )


@pytest.mark.feature("snapshots")
def test_repeated_create_restore_cycles(snap_nest):
    """The originally reported repro verbatim: create→restore, four times.

    This is the shape that broke `test_backups_restore.py` — cycle 0 created and
    restored fine, then cycle 1's *create* raised `snapshot.internal` because it
    landed in the same second as cycle 0's.

    Unlike the test above, this one does **not** guarantee it lands two creates
    in one second: a cycle opens two WS connections and does a restore, so on a
    loaded box the cycles may straddle second boundaries. It is kept as the
    journey-level regression of the exact reported failure — the same-second
    mechanism itself is guaranteed-exercised by
    `test_same_second_repeat_creates_dedup_to_one_snapshot` above and by the
    DAO tests in `bins/fauna-nest/src/db/snapshots.rs`.
    """
    owner = create_actor_and_register(
        snap_nest["port"], admin_signing_key=snap_nest["admin"]["signing_key"]
    )

    for cycle in range(4):
        with _ws_client(snap_nest, owner) as ws:
            reply = ws.call(
                "fauna.filesync.snapshot.create_message_kind",
                {"kind": "mail"},
            )
            snapshot_id = reply["snapshot_id"]
            assert isinstance(snapshot_id, int) and snapshot_id > 0, (
                f"cycle {cycle}: create returned {reply!r}"
            )

        with _ws_client(snap_nest, owner) as ws:
            restore = ws.call(
                "fauna.filesync.snapshot.restore_message_kind",
                {"snapshot_id": snapshot_id, "confirm_id": str(snapshot_id)},
            )
            assert restore["snapshot_id"] == snapshot_id, (
                f"cycle {cycle}: restore returned {restore!r}"
            )
