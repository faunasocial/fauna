"""Nest-trust facet — the backup and recovery rows' HONESTY states: an
unreachable destination, the seal revoke, a path-less retained version, and a
restore past the recovery window (`docs/goal/ui/nests.md` § Trust facet —
backup rows, § Trust facet — generation recovery).

`test_nest_trust.py` proves the happy paths (both rows render active, the
writer revoke lands at the destination, a listed generation restores). These
journeys prove the states the copy must keep apart — the ones that exist
because collapsing them is false reassurance: *could not reach* is not
*missing* and not *nothing to recover*; a row with no readable path is still a
row; *too late* is not *broken*.

Every test runs as a dedicated fresh actor (`dedicated_actor_app`) against its
OWN destination nest: the seal revoke is durable for its owner, a destination
is stopped mid-test, and generation rows are rewritten and reclaimed — none of
which may reach the session's shared `test_user` or `second_nest`.

tier_3 (full stack). Standalone-only in practice: stopping a destination uses
the harness's own process handle (`common.nest.stop_nest`), and the past-window
restore moves the destination's reclaim clock through its `test-hooks` route.
"""
import sqlite3
import time

import pytest
import requests

from common.auth import register_user
from common.nest import start_nest_in_place, stop_nest
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

_DAY = 24 * 60 * 60

# Named ceilings for states that need a live round trip per destination (and,
# for an unreachable one, the connect failure) — deadline polls, never
# settle-sleeps (convention 14).
_UI_S = 30.0


@pytest.fixture
def trust_destination(request, nest_mode, tmp_path_factory):
    """A dedicated destination nest per test, so stopping it, rewriting its
    generation rows or reclaiming them touches nothing any other test reads."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "trust-destination")
    yield nest
    cleanup()


def _enroll(app, owner, destination, name: str) -> None:
    """Register `owner` at the destination (a v1 destination is a nest the
    owner administers) and enroll it through the Backups page UI — which is
    what mints the seal grant at the source and the writer grant there."""
    register_user(
        destination["port"],
        owner["actor_id_hex"],
        admin_signing_key=destination["admin"]["signing_key"],
    )
    app.backups.remove_every_destination()
    app.backups.add_destination(destination["url"], name=name)
    app.backups.wait_for_destination_count(1)


def _sweep(nest_instance) -> None:
    """One synchronous source-nest backup pass — the causal barrier (the
    `test_nest_trust.py` sweep)."""
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/backup/run-now", json={}, timeout=60
    )
    assert resp.status_code == 200, f"backup run-now: {resp.status_code} {resp.text}"
    payload = resp.json()
    assert payload.get("ok") is True and payload.get("owners_run", 0) >= 1, (
        f"the sweep ran no owners ({payload!r}) — the enroll never reached the nest"
    )


def _db_rows(nest, sql: str, params=()) -> list:
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        return conn.execute(sql, params).fetchall()
    finally:
        conn.close()


def _poll(predicate, what, budget_s: float = _UI_S):
    """Deadline-poll `predicate`. `what` is the failure text — or a callable
    producing it, read AFTER the budget runs out so it can say what the page
    last showed (convention 6)."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.5)
    raise AssertionError(f"never observed: {what() if callable(what) else what}")


def _supersede_one_generation(app, nest_instance, owner) -> None:
    """Upload a mail segment, grow it, upload again — the second pass
    re-records the same path with a new manifest, so the destination RETAINS
    the first generation (`test_nest_trust.py`'s retained-generation setup)."""
    from tests.test_backups import _seed_one_mail_segment

    _seed_one_mail_segment(app, nest_instance, owner)
    _sweep(nest_instance)
    _seed_one_mail_segment(app, nest_instance, owner)
    _sweep(nest_instance)


@pytest.mark.feature("nests-and-trust")
def test_an_unreachable_destination_reads_unreachable_never_missing_or_nothing_to_recover(
    dedicated_actor_app, trust_destination
):
    """Outcomes 14 and 16 (first half): with the destination down, its writer
    row reads *could not reach this destination* — never *missing* — and the
    recovery list shows one *could not reach* row with no restore, never an
    empty "nothing to recover".

    `nests.md` § Trust facet — backup rows: *An unreachable destination renders
    `status: unreachable`; a configured destination whose writer list lacks the
    home nest renders `status: missing`.* § generation recovery, state 1: *A
    failed read is not an empty one* — enforced in shared Rust by
    `GenerationsStatus::Unreachable`; this proves the shell renders it.
    """
    app, owner = dedicated_actor_app
    app.nest_trust.require_backup_trust_rows_supported()
    app.nest_trust.require_retained_generations_supported()
    destination = trust_destination

    _enroll(app, owner, destination, "Offline")
    app.linked_nests.navigate()
    assert app.nest_trust.wait_for_backup_row_count(2), (
        f"expected the seal + writer rows, got {app.nest_trust.backup_row_count()}. "
        f"error: {app.error_text()!r}"
    )
    assert app.nest_trust.backup_status_text(1) == S.nests.backup_status_active

    stop_nest(destination, graceful=True)
    try:
        def writer_unreachable() -> bool:
            # The read behind these rows is per VISIT, so a stale "active" row means
            # re-enter the page — but never while a visit's own load is still in
            # flight: with the destination down that load is a connect attempt that
            # outlasts this poll's interval, and re-entering every interval restarted
            # it every time, so on an app whose page re-entry re-fetches (windows)
            # the rows never finished rendering and the poll read 0 rows for good.
            if app.nest_trust.backup_row_count() < 2:
                return False
            if app.nest_trust.backup_status_text(1) == S.nests.backup_status_unreachable:
                return True
            app.linked_nests.navigate()
            return False

        _poll(
            writer_unreachable,
            lambda: "the writer row reading unreachable — last seen "
            f"{app.nest_trust.backup_row_count()} row(s), writer status "
            f"{app.nest_trust.backup_status_text(1)!r}, error {app.error_text()!r}",
        )
        assert app.nest_trust.backup_status_text(1) != S.nests.backup_status_missing
        # The seal grant lives at the SOURCE, which is up: untouched.
        assert app.nest_trust.backup_status_text(0) == S.nests.backup_status_active

        # Recovery: exactly the one could-not-reach row, and no restore on it.
        assert app.nest_trust.wait_for_generation_count(1), (
            "a destination that could not be asked must still render a row — an "
            "empty recovery list here is the false 'nothing to recover'"
        )
        assert app.nest_trust.generation_count() == 1
        assert app.nest_trust.generation_status_text(0) == (
            S.nests.generation_status_unreachable
        ), f"got {app.nest_trust.generation_status_text(0)!r}"
        assert not app.nest_trust.has_generation_restore(0), (
            "an unreachable row has no address to restore — offering one would "
            "imply knowledge we do not have"
        )
    finally:
        start_nest_in_place(destination)

    app.backups.navigate()
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)


@pytest.mark.feature("nests-and-trust")
def test_stopping_the_seal_stops_new_backups_and_leaves_held_copies(
    dedicated_actor_app, nest_instance, trust_destination
):
    """Outcome 15: the owner can stop their nest sealing new backups from the
    seal row; copies the destination already holds stay there.

    `nests.md` § Trust facet — backup rows, row 1: *Revoke dispatches
    `fauna.backup.nest_key.revoke` (the nest can no longer seal new segments;
    existing destination custody is untouched).* The seal row renders only
    while `fauna.backup.status` reports enrolled, so after the revoke it is
    gone — and the writer row, a different grant held at the destination,
    stays active.
    """
    app, owner = dedicated_actor_app
    app.nest_trust.require_backup_trust_rows_supported()
    destination = trust_destination
    owner_id = bytes.fromhex(owner["actor_id_hex"])

    _enroll(app, owner, destination, "Kept")
    from tests.test_backups import _seed_one_mail_segment

    _seed_one_mail_segment(app, nest_instance, owner)
    _sweep(nest_instance)
    held_before = _db_rows(destination, "SELECT COUNT(*) FROM backup_custody")[0][0]
    assert held_before >= 1, "the sweep must have left custody at the destination"

    app.linked_nests.navigate()
    assert app.nest_trust.wait_for_backup_row_count(2)
    assert S.nests.backup_scope_seal in app.nest_trust.backup_scope_text(0)
    assert app.nest_trust.backup_bound_note_text(0) == S.nests.backup_bound_note_seal

    app.nest_trust.revoke_backup(index=0)

    def seal_row_gone() -> bool:
        app.linked_nests.navigate()
        return (
            app.nest_trust.backup_row_count() == 1
            and "Kept" in app.nest_trust.backup_scope_text(0)
        )

    _poll(seal_row_gone, "the seal row leaving, the writer row staying")
    assert app.nest_trust.backup_status_text(0) == S.nests.backup_status_active, (
        "stopping the seal must not touch the destination's writer grant"
    )

    # The source nest no longer holds the key it sealed with …
    assert _db_rows(
        nest_instance,
        "SELECT COUNT(*) FROM nest_backup_keys WHERE owner_actor_id = ?",
        (owner_id,),
    )[0][0] == 0, "the seal revoke must delete this owner's NestBackupKey at the source"
    # … and what the destination already held is still held.
    held_after = _db_rows(destination, "SELECT COUNT(*) FROM backup_custody")[0][0]
    assert held_after == held_before, (
        f"held copies must stay until reclaimed: {held_before} before, {held_after} after"
    )

    app.backups.navigate()
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)


@pytest.mark.feature("nests-and-trust")
def test_a_path_less_version_is_listed_and_a_restore_past_the_window_says_so(
    dedicated_actor_app, nest_instance, trust_destination
):
    """Outcomes 16 (second half) and 17: a retained version whose custody
    carries no readable path is still listed — by its hash, restorable — and
    restoring a version the destination has since reclaimed past its window
    says *past the recovery window*, never a failure.

    `nests.md` § generation recovery, states 2 and 3. The path-less row is
    produced the way it arises — a custody row whose path was scrubbed (sealed)
    or never supplied, i.e. `path IS NULL` on the destination. The
    reclaim is the destination's production statement, with its clock moved
    past `T` = 30 d through `POST /api/v1/test/backup/reclaim-generations`
    (convention 14 — a fake clock, never a month's sleep).
    """
    app, owner = dedicated_actor_app
    app.nest_trust.require_retained_generations_supported()
    destination = trust_destination

    _enroll(app, owner, destination, "Rewind")
    _supersede_one_generation(app, nest_instance, owner)

    retained = _db_rows(
        destination, "SELECT path_hash FROM backup_custody_generations"
    )
    assert len(retained) == 1, f"expected one retained generation, got {retained!r}"
    path_hash_hex = bytes(retained[0][0]).hex()
    conn = sqlite3.connect(destination["db_path"], timeout=10.0)
    try:
        conn.execute("UPDATE backup_custody_generations SET path = NULL")
        conn.commit()
    finally:
        conn.close()

    app.linked_nests.navigate()
    assert app.nest_trust.wait_for_generation_count(1), (
        f"the retained version never rendered. error: {app.error_text()!r}"
    )
    assert app.nest_trust.generation_status_text(0) == S.nests.generation_status_listed
    path_text = app.nest_trust.generation_path_text(0)
    assert path_hash_hex[:8] in path_text, (
        "a path-less version renders its path_hash in the path's place — never "
        f"hidden; expected {path_hash_hex[:8]!r} in {path_text!r}"
    )
    assert app.nest_trust.has_generation_restore(0), (
        "only the DISPLAY path is missing — the version is still restorable"
    )

    # The destination's window elapses (the row on screen is now stale).
    resp = requests.post(
        f"{destination['url']}/api/v1/test/backup/reclaim-generations",
        json={"now_offset_secs": 31 * _DAY},
        timeout=30,
    )
    assert resp.status_code == 200, f"reclaim hook: {resp.status_code} {resp.text}"
    assert resp.json().get("reclaimed", 0) >= 1, resp.json()

    app.nest_trust.restore_generation(0)

    def notice() -> str:
        return app.nest_trust.generation_notice_text().strip()

    _poll(lambda: bool(notice()), "a restore outcome on nest-trust-generation-notice")
    assert notice() == S.nests.generation_past_window, (
        f"a reclaimed version must say it is past the recovery window; got "
        f"{notice()!r} (error={app.error_text()!r})"
    )
    assert not app.error_text().strip(), (
        "past the window is a product state, never an error — error-message must "
        f"stay empty; got {app.error_text()!r}"
    )

    app.backups.navigate()
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)
