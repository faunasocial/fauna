"""tier_3: a nest that went back to an older copy of itself gets its lost
events, contacts and posts back from its nest backup destination, each once,
and an event or contact changed since the regression keeps that change
(`docs/features/backup-destinations-and-restore.md` outcome 26).

The recovery is client Rust — the audit pass that accepts the regression and
the shared driver `fauna_sync_engine::recovery::recover_from_destination` over
the nest's `fauna.backup.custody.recover` (`client-device-custodian.md`
§ Implementation status today, the calendar and contacts arms and
`segments::post::reassert_records`) — so, per `feature-catalog.md` § The two
surfaces, it is driven app-less through the harness library
(`harness_backup_audit_pass`, `harness_backup_recover`), never a Python
re-implementation of a leg. The mail arm has its app journey in
`tests/test_backups.py` (`test_a_nest_that_went_back_in_time_gets_its_lost_mail_back_from_the_backup`);
this module covers the other three kinds, each its own case.

The regression is real, not injected — the tier_3 Rust recipe
(`nest_backup_coordinator.rs`, `a_regressed_source_recovers_its_lost_events_by_record`)
reproduced over dedicated nests::

    box A (owner), box D (destination, owner registered)
    enroll D (harness); one kept item; push       -> D holds A's copy era
    stop A, copy its data dir, restart A
    a lost item; push; an item to be edited; push -> D holds them too
    audit pass (+25 h)                            -> this device pins D's ledger
    stop A, restore the copy, restart A           -> A lost both items
    the edited item put again with a later body,
    or a fresh post; push                         -> A's counter reuses a lost id
    audit pass (+50 h)                            -> regression accepted: recover offered
    harness_backup_recover                        -> whole
    read back on A                                -> kept + lost + edit, each once;
                                                     the edit's later body stands
    audit pass (+75 h)                            -> nothing left to offer

Every wait is a synchronous barrier (the run-now sweep, each leg's own return,
the nest restart's readiness) or a deadline on the state it waits for, never a
sleep (convention 14); the only time values are explicit audit-clock offsets.
"""

import os
import secrets
import time

import pytest

import fauna_ffi
from common.auth import register_user
from helpers.backup_seed import (
    author_feed_ids,
    create_post,
    dav_items_served,
    post_body,
    provision_dav_collection,
    push_backup,
    put_dav_item,
)

pytestmark = pytest.mark.tier_3

_HOUR = 60 * 60


def _segments(nest: dict, owner: dict, kind: str) -> dict:
    """The owner's segment listing of ``kind`` on ``nest``: the saved counter
    and the live ids — the nest-side half of the regression precondition."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    ) as client:
        reply = client.call(
            "fauna.segments.list", {"kind": kind, "actor_id": owner["actor_id_hex"]})
    return {
        "next_segment_id": reply.get("next_segment_id"),
        "ids": sorted(s["segment_id"] for s in reply.get("segments", [])),
    }


def _next_second(after: int) -> None:
    """Wait until the wall clock's second is past ``after``: receive times are
    whole seconds, and the owner's post-regression act must be born strictly
    after every lost record, as it is in life. A deadline on the clock's own
    state, never a fixed sleep (convention 14) — the nest restart in between
    has nearly always carried it past already."""
    deadline = time.monotonic() + 5
    while int(time.time()) <= after:
        assert time.monotonic() < deadline, "the wall clock did not advance"
        time.sleep(0.05)


class _Dav:
    """A calendar or an address book: the kept item, the lost item, and the
    item lost at one body and put again after the regression at a later one."""

    def __init__(self, dav: str, seal):
        self.dav, self.seal = dav, seal
        self.kept_uid, self.lost_uid, self.edited_uid = (
            secrets.token_bytes(32) for _ in range(3))

    def copy_era(self, box, owner):
        self.collection = provision_dav_collection(box, owner, self.seal, self.dav)
        self.kept = put_dav_item(
            box, owner, self.seal, self.dav, self.collection, self.kept_uid, "kept")

    def lost_era(self, box, owner):
        self.lost = put_dav_item(
            box, owner, self.seal, self.dav, self.collection, self.lost_uid, "lost")
        push_backup(box)
        self.edited_lost = put_dav_item(
            box, owner, self.seal, self.dav, self.collection, self.edited_uid, "edited v1")

    def after_regression(self, box, owner):
        assert dav_items_served(box, owner, self.dav, self.collection) == [
            (self.kept_uid, self.kept)], "pre-state: the restored copy holds only the kept item"
        self.edit = put_dav_item(
            box, owner, self.seal, self.dav, self.collection, self.edited_uid,
            "edited v2, after the regression")

    def verify(self, box, owner):
        served = dav_items_served(box, owner, self.dav, self.collection)
        uids = [u for u, _ in served]
        assert sorted(uids) == sorted([self.kept_uid, self.lost_uid, self.edited_uid]), (
            f"NEST-SIDE: the {self.dav} collection must serve the kept, the lost and "
            f"the edited item, each exactly once; it serves {len(served)} rows "
            f"({len(set(uids))} distinct)")
        bodies = dict(served)
        assert bodies[self.lost_uid] == self.lost, f"the lost {self.dav} item is back verbatim"
        assert bodies[self.kept_uid] == self.kept, f"the kept {self.dav} item is untouched"
        assert bodies[self.edited_uid] == self.edit, (
            f"the {self.dav} item changed after the regression keeps that change: the "
            f"recovery must not put its lost earlier body back over it")
        assert bodies[self.edited_uid] != self.edited_lost


class _Posts:
    """Posts: the kept post, two lost posts, and one written after the regression."""

    dav = "post"

    def __init__(self, _dav, _seal):
        pass

    def copy_era(self, box, owner):
        self.kept = create_post(box, owner, f"kept {secrets.token_hex(6)}")

    def lost_era(self, box, owner):
        self.lost = [create_post(box, owner, f"lost one {secrets.token_hex(6)}")]
        push_backup(box)
        self.lost.append(create_post(box, owner, f"lost two {secrets.token_hex(6)}"))

    def after_regression(self, box, owner):
        held = author_feed_ids(box, owner)
        assert held == [self.kept[0]], f"pre-state: the restored copy holds only the kept post: {held!r}"
        self.after = create_post(box, owner, f"after the regression {secrets.token_hex(6)}")

    def verify(self, box, owner):
        for post_id, body in [self.kept, *self.lost, self.after]:
            assert post_body(box, owner, post_id) == body, (
                f"NEST-SIDE: post {post_id} must read back verbatim by id")
        feed = author_feed_ids(box, owner)
        expected = {self.kept[0], self.after[0], *(p for p, _ in self.lost)}
        assert sorted(feed) == sorted(expected), (
            f"the author feed must list every post exactly once: {feed!r}, "
            f"expected {sorted(expected)!r}")


# The `card` case is also the witness that contact cards ride the off-box copy
# (`address-book.md` outcome 5, `carddav-server.md` § Storage model): box D holds
# the lost card only because the source's backup sweep pushed it there.
@pytest.mark.feature("backup-destinations-and-restore", "address-book")
@pytest.mark.parametrize("kind", ["calendar", "card", "post"])
def test_a_regressed_nest_recovers_its_lost_records_once_and_keeps_a_later_edit(
    kind, request, nest_mode, tmp_path_factory, tmp_path, run_seal_helper
):
    from common.nest import copy_data_dir, restore_data_dir, start_nest_in_place, stop_nest
    from conftest import _start_dedicated_nest

    box_a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, f"regress-{kind}-src")
    request.addfinalizer(cleanup_a)
    box_d, cleanup_d = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, f"regress-{kind}-dst")
    request.addfinalizer(cleanup_d)
    owner = box_a["admin"]
    secret = bytes(owner["signing_key"])
    register_user(
        box_d["port"], owner["actor_id_hex"], admin_signing_key=box_d["admin"]["signing_key"])
    state_path = str(tmp_path / "audit-state.cbor")
    seeded = (_Posts if kind == "post" else _Dav)(kind, run_seal_helper)

    fauna_ffi.harness_enable_mail(box_a["url"], secret)
    destination_id, _rows = fauna_ffi.harness_backup_enroll(
        box_a["url"], secret, box_d["url"], "Off-site")
    seeded.copy_era(box_a, owner)
    push_backup(box_a)

    # ── The copy, taken while A is stopped so it is consistent ───────────────
    copy_dir = str(tmp_path_factory.mktemp(f"regress-{kind}-copy") / "data")
    stop_nest(box_a, graceful=True)
    request.addfinalizer(copy_data_dir(box_a, copy_dir))
    start_nest_in_place(box_a)
    at_copy = _segments(box_a, owner, kind)

    seeded.lost_era(box_a, owner)
    push_backup(box_a)
    last_lost = int(time.time())
    before_rollback = _segments(box_a, owner, kind)
    assert before_rollback["next_segment_id"] > at_copy["next_segment_id"], (
        f"the lost items must sit in segments the copy does not hold, or nothing is "
        f"lost: at the copy {at_copy!r}, before the rollback {before_rollback!r}")

    pinned = fauna_ffi.harness_backup_audit_pass(
        box_a["url"], secret, state_path, 25 * _HOUR)
    assert [r["destination_id"] for r in pinned] == [destination_id], pinned
    assert not pinned[0]["recover_offered"], f"no regression yet: {pinned!r}"

    # ── The regression ───────────────────────────────────────────────────────
    stop_nest(box_a, graceful=True)
    restore_data_dir(box_a, copy_dir)
    start_nest_in_place(box_a)
    _next_second(last_lost)
    seeded.after_regression(box_a, owner)
    push_backup(box_a)
    after_rollback = _segments(box_a, owner, kind)
    assert after_rollback["next_segment_id"] < before_rollback["next_segment_id"], (
        f"A's saved counter must read below what D's copy numbered, or the device "
        f"sees no regression: before {before_rollback!r}, after {after_rollback!r}")

    accepted = fauna_ffi.harness_backup_audit_pass(
        box_a["url"], secret, state_path, 50 * _HOUR)
    assert accepted and accepted[0]["recover_offered"], (
        f"the pass must accept the regression and offer the recovery from D: {accepted!r}")
    assert os.path.exists(state_path)

    # ── The recovery ─────────────────────────────────────────────────────────
    recovered = fauna_ffi.harness_backup_recover(box_a["url"], secret, destination_id, state_path)
    assert recovered["whole"] is True, f"the recovery must be whole: {recovered!r}"
    seeded.verify(box_a, owner)

    # A later pass finds nothing left to recover, and a second run lands nothing twice.
    after = fauna_ffi.harness_backup_audit_pass(box_a["url"], secret, state_path, 75 * _HOUR)
    assert not after[0]["recover_offered"], f"the recovery cleared the notice: {after!r}"
    seeded.verify(box_a, owner)
