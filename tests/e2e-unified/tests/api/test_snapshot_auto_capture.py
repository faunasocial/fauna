"""Automatic snapshots — full-stack (tier_3) API E2E.

Witnesses `snapshots` outcome 13, *"A folder you changed and then left alone
gets a snapshot by itself, with nothing asked of you"*
(``docs/goal/behavior/backup-restore.md`` § Automatic Snapshots). Until this
file the scheduler's automatic capture — the whole promise that a user who
never presses anything still has restore points — was pinned only by
in-process Rust unit tests over ``CacheDb::folder_needs_snapshot``; no test
anywhere drove ``SnapshotScheduler::check_all_folders`` against a real nest,
so a regression that stopped the *scheduler* calling it (rather than the
predicate answering wrongly) would have been invisible.

**Latency-independent** (e2e-conventions.md convention 14). The production
scheduler ticks every ``backup::scheduler::CHECK_INTERVAL`` (60 s) with a
nest-wide ``DEFAULT_QUIET_SECS`` of 30 — so waiting for a real tick would be
both slow and a wall-clock assertion. ``POST
/api/v1/test/snapshot_scheduler/run-now``
(``bins/fauna-nest/src/snapshot_scheduler_test_hook.rs``) runs exactly one
``check_all_folders`` pass *synchronously* and only then replies, built from
those same two production constants — so when it returns, the capture has
either happened or definitively not, and the folder's own
``nest_place.quiet_secs`` is what decided it, exactly as in production.

**Two-way discriminator, not an assertion by existence.** "Any folder gets a
snapshot" would pass the positive arm just as happily, and would be a false
reassurance about a promise whose whole point is that the nest respects the
owner's choice. So the control arm is a second folder, changed identically,
whose owner set ``nest_place.snapshots = false``: the same pass must leave it
untouched. (Verified by mutation: forcing ``folder_needs_snapshot`` to ignore
the opt-out reds the control arm.)

Process safety: no ``pkill``/``killall``; the nest is the session-scoped
``nest_instance`` and is torn down by its own fixture.
"""

from __future__ import annotations

import secrets

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register, sync_changes_record, sync_register, user_create_folder

pytestmark = pytest.mark.tier_3


def _ws(nest_instance, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _snapshot_rows(nest_instance, actor, folder) -> list[dict]:
    with _ws(nest_instance, actor) as ws:
        return ws.call("fauna.filesync.snapshot.list", {"folder": folder})["rows"]


def _set_nest_place(nest_instance, actor, folder, *, snapshots: bool, quiet_secs: int) -> None:
    """The owner's per-folder nest-place choice, over the production kind the
    Settings → Folders UI drives (``fauna.folders.update``). Sent whole and
    applied whole: both knobs ride together or a knob left out clears back to
    the nest-wide default (``NestPlacePolicy``'s own contract)."""
    with _ws(nest_instance, actor) as ws:
        ws.call(
            "fauna.folders.update",
            {
                "name": folder,
                "nest_place": {"snapshots": snapshots, "quiet_secs": quiet_secs},
            },
        )


def _changed_folder(nest_instance, actor, device_id, *, snapshots: bool) -> str:
    """A fresh owned folder holding one recorded change and no snapshot — the
    exact state § Automatic Snapshots describes as owing one — with the
    owner's nest-place choice applied.

    ``quiet_secs = 0`` is the owner saying "cut as soon as I stop": it is the
    per-folder override `folder_needs_snapshot` resolves against the nest-wide
    30 s, so the folder is due the moment the change lands. Without it the
    assertion below would be a 30-second wall-clock wait, which convention 14
    forbids — the override is the production knob that makes the promise
    observable, not a test backdoor.
    """
    name = f"autosnap-{secrets.token_hex(4)}"
    user_create_folder(nest_instance["port"], name, secret_key=bytes(actor["signing_key"]).hex())
    sync_changes_record(
        nest_instance["port"],
        secret_key=bytes(actor["signing_key"]).hex(),
        folder=name,
        device_id=device_id,
        path=f"notes/{secrets.token_hex(4)}.txt",
        manifest_hash=secrets.token_bytes(32).hex(),
        size_bytes=128,
    )
    # Asserted BEFORE the quiet-period flip, deliberately: the production
    # scheduler is running on this nest too, so while the folder still rests on
    # the nest-wide 30 s quiet it cannot be due, and no background tick can
    # slip a snapshot in between the record above and this read. Flipping
    # first would leave that window open on every run.
    assert _snapshot_rows(nest_instance, actor, name) == [], (
        f"folder {name} must start with no snapshot — the pass below is what "
        f"has to create one"
    )
    _set_nest_place(nest_instance, actor, name, snapshots=snapshots, quiet_secs=0)
    return name


def _run_scheduler_pass(nest_instance) -> dict:
    """One synchronous ``check_all_folders`` pass — the causal barrier."""
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/snapshot_scheduler/run-now",
        json={},
        timeout=60,
    )
    assert resp.status_code == 200, (
        f"test-hooks snapshot_scheduler run-now returned {resp.status_code}: {resp.text}"
    )
    payload = resp.json()
    assert payload.get("ok") is True, payload
    return payload


@pytest.mark.feature("snapshots")
def test_a_changed_then_quiet_folder_snapshots_itself(nest_instance):
    """The nest cuts the snapshot; the owner asked for nothing.

    The captured row is asserted to be a *scheduler* capture and not a
    client-created one: `check_all_folders` calls `create_snapshot_v2(.., None,
    &[], None, None, parent)` — no device, no tags — because the nest names
    nothing and holds no key to seal a display copy with. A row carrying a
    `device_id` would mean some client path created it instead, which is the
    one way this test could pass while the promise stayed broken.
    """
    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    device_id = secrets.token_hex(32)
    sync_register(
        nest_instance["port"],
        secret_key=bytes(actor["signing_key"]).hex(),
        device_id=device_id,
    )

    kept = _changed_folder(nest_instance, actor, device_id, snapshots=True)
    opted_out = _changed_folder(nest_instance, actor, device_id, snapshots=False)

    _run_scheduler_pass(nest_instance)

    rows = _snapshot_rows(nest_instance, actor, kept)
    assert len(rows) == 1, (
        f"a changed, quiet folder must be snapshotted by the nest itself, got "
        f"{len(rows)} snapshot(s): {rows!r}"
    )
    assert rows[0]["device_id"] is None, (
        "an automatic snapshot is unattributed — the nest names nothing and "
        f"seals no display copy; got device_id={rows[0]['device_id']!r}, which "
        "means a client path created this row instead of the scheduler"
    )
    assert rows[0]["message_kind"] is None, rows[0]
    assert rows[0]["soft_deleted"] is False and rows[0]["deletion_pending"] is False, rows[0]

    assert _snapshot_rows(nest_instance, actor, opted_out) == [], (
        "a folder whose owner turned the nest place's snapshots OFF must be "
        "left alone by the same pass — otherwise the positive arm above only "
        "shows that the nest snapshots everything"
    )
