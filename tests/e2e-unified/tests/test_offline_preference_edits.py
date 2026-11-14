"""An offline preference edit is kept across a restart and reaches the nest; a
device that has not finished its first sync refuses one and says it needs the nest.

Owners:

* ``docs/goal/architecture/account-offline-mutation.md`` § The offline-mutation
  contract, discriminator 1: an ``OfflineSafe`` mutation IS a store write —
  its journal row is its durable replay record, published by own-log replay
  above the published watermark when the nest is back. A muted word is one
  latest-wins ``fauna.state.moderation`` row, so it is that class.
* ``docs/goal/architecture/account-client-lifecycle.md`` § Implementation status
  today, the first-listing gate: a replica that has not listed a scope refuses
  the read (``ScopeNotReady``), the plane-only preference surfaces answer it as
  ``common.needs_nest``, and tui shows it on ``error-message`` for a refused
  muted-words load and for every refused gesture.
* ``docs/features/offline-aware-controls.md`` outcomes 6 and 7.

**Why a muted word, not an ``OfflineQueued`` change.** This file witnesses outcome
6 on the offline-safe class — a store write, whose durability across a restart is
the same promise to the user. The queued class has its own witness,
``test_offline_queued_like.py`` (a like composed through the outbox).

**How "this device before its first sync" is reached.** A relaunch carries the
signed-in actor's writer key AND its replica (convention 10, the principal
carry), so the second test asks the driver to withhold the replica once
(``wipe_replica_at_next_relaunch``): with the nest held down the relaunched seat
holds a replica that has never listed the account — the engine's
``a_listed_replica_restarted_offline_reads_and_writes_and_a_wiped_one_does_not``
case, seen from the app. The pinned relaunch in the first test is the other
half of that pair: a listed replica, restarted offline, still reads and writes.

tier_3: the real nest, taken down and brought back on the same data dir; real
seats; every mutation a UI gesture (convention 8); every wait a deadline poll on
painted state (convention 14). tui first (``testing.md`` § Default app and nest
mode); the other apps add their marker as their trickle-down batch reaches them.
The restart half also runs on linux, which hosts its account runtime at session
establishment as tui does, so a launch with the nest down still reads and writes
its listed replica. The first-sync refusal also runs on linux, whose relaunch carries the principal
and its replica as tui's does, so ``wipe_replica_at_next_relaunch`` withholds a
real replica there. It runs on android too, whose replica never leaves the
device: the app's filesDir outlives the relaunch, so the driver's arm deletes
``files/account-store/<actor>/account-store`` inside the app sandbox while the
app is stopped, and the replayed sign-in finds no replica to adopt. web has its
driver arm (it deletes the actor's account store, its IndexedDB database and
OPFS directory, which a reload keeps, from a same-origin page that runs no app,
after closing the SPA's own, while the registry's writer key stays) but does not
join yet: a tab loaded with the nest unreachable hosts no account runtime,
because the runtime starts only once the conversations manager is built and that
build waits for the socket, so the page reads "the account runtime is not
running" instead of the first-listing refusal. The other apps need a
replica-wipe arm of their own drivers before they join.
"""
from __future__ import annotations

import time

import pytest

from common.nest import start_nest_in_place
from helpers.fleet import SiblingSeats, poked_pass
from helpers.nest_outage import hold_nest_down
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

#: A seat's own add round trip, nest up or down (a local store write).
ADD_S = 120.0

#: The first-listing gate's own bound is ``FIRST_PASS_WAIT`` (30 s,
#: ``handle_source.rs``); the refusal is painted after it, so this sits well
#: above it.
REFUSAL_S = 180.0

#: The returning nest, the restarted seat's publish, and a second seat's walk.
CONVERGE_S = 300.0


@pytest.fixture
def sibling_seats(request, nest_instance):
    """Second enrolled seats on the session nest (`helpers.fleet.SiblingSeats`)."""
    seats = SiblingSeats(request, nest_instance)
    try:
        yield seats
    finally:
        seats.teardown()


def _stamp() -> int:
    return int(time.time() * 1000) % 1_000_000


def _signed_in_with_a_listed_word(app, request, nest_instance, word: str) -> dict:
    """A fresh account on ``app``, enrolled, with ``word`` added while the nest
    is up — so the seat has finished its first listing and the account holds a
    record of its own."""
    from conftest import _make_user
    from helpers import enrollment

    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_instance["url"], user)
    app.muted_words.navigate()
    app.muted_words.add(word)
    assert app.muted_words.wait_for_word(word, timeout=ADD_S), (
        f"the online add never listed {word!r} (rows {app.muted_words.words()}, "
        f"error-message {app.error_text()!r}) — nothing below can be trusted"
    )
    return user




@pytest.mark.feature("offline-aware-controls")
@pytest.mark.tui
@pytest.mark.linux
def test_an_offline_change_is_kept_across_restart_and_reaches_the_nest(
    app, request, nest_instance, sibling_seats
):
    stamp = _stamp()
    online_word, offline_word = f"onl{stamp:06d}", f"off{stamp:06d}"
    user = _signed_in_with_a_listed_word(app, request, nest_instance, online_word)

    # The restart must be THIS device restarting, not a fresh one: without the
    # pin a relaunch gets an empty replica and "the word did not survive" would
    # be indistinguishable from "a new device that never had it".
    assert app.driver.preserve_state_across_relaunch(), (
        "the app could not pin its client store across the relaunch, so the restart "
        "half of this outcome would be vacuous"
    )

    hold_nest_down(app, nest_instance, act="an offline edit below")
    try:
        app.muted_words.navigate()
        app.muted_words.add(offline_word)
        assert app.muted_words.wait_for_word(offline_word, timeout=ADD_S), (
            f"an offline-safe edit was not applied with the nest down: rows "
            f"{app.muted_words.words()}, error-message {app.error_text()!r}"
        )

        # Close the app and open it again, the nest still down: the edit lives
        # only in this device's store now.
        app.driver.hard_reload()
        app.muted_words.navigate()
        assert app.muted_words.wait_for_word(offline_word, timeout=ADD_S), (
            f"after a restart with the nest still down the offline edit is gone: "
            f"rows {app.muted_words.words()}, error-message {app.error_text()!r} — "
            "the store write was not durable, or the restarted seat refused its "
            "own listed replica"
        )
    finally:
        start_nest_in_place(nest_instance)

    # The nest is back. The only way a second device can learn the word is
    # through the nest, so seeing it there proves the restarted seat published it.
    from helpers import enrollment

    seat_b = sibling_seats.launch(user=user)
    enrollment.await_enrollment(seat_b, nest_instance["url"], user)
    seat_b.muted_words.navigate()
    seen: dict[str, list[str]] = {}

    def second_seat_holds_it() -> bool:
        poked_pass(app.driver, what="the restarted seat's publish")
        poked_pass(seat_b.driver, what="the second seat's walk")
        seen["b"] = seat_b.muted_words.revisit()
        return {online_word, offline_word} <= set(seen["b"])

    wait_until(
        second_seat_holds_it, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"a second device lists {seen.get('b')} after the nest came back; it "
            f"must hold {offline_word!r} (made offline, kept across the restart) "
            f"and {online_word!r}. Restarted seat rows {app.muted_words.revisit()}, "
            f"error-message A={app.error_text()!r} B={seat_b.error_text()!r}"
        ),
    )


@pytest.mark.feature("offline-aware-controls")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
def test_a_device_before_its_first_sync_says_it_needs_the_nest_and_refuses_edits(
    app, request, nest_instance
):
    stamp = _stamp()
    kept_word, refused_word = f"kpt{stamp:06d}", f"ref{stamp:06d}"
    user = _signed_in_with_a_listed_word(app, request, nest_instance, kept_word)
    assert app.driver.wipe_replica_at_next_relaunch(user["actor_id_hex"]), (
        f"{app.driver.__class__.__name__}'s relaunch carries no replica to "
        "withhold, so the relaunch below "
        "would not be a never-listed replica for the reason this test names"
    )

    hold_nest_down(app, nest_instance, act="an offline edit below")
    try:
        # The relaunched seat keeps its writer key but not its replica: a store
        # that has never listed the account, and no nest to list it from.
        app.driver.hard_reload()
        app.muted_words.navigate()

        def says_it_needs_the_nest() -> bool:
            return S.common.needs_nest in (app.error_text() or "")

        wait_until(
            says_it_needs_the_nest, REFUSAL_S, interval=1.0,
            diagnose=lambda: (
                f"the Muted words page of a never-listed replica reads "
                f"error-message {app.error_text()!r}, rows "
                f"{app.muted_words.words()}; it must say {S.common.needs_nest!r}"
            ),
        )
        # negative-visibility-ok: is_empty_state_visible reads driver.is_absent.
        assert not app.muted_words.is_empty_state_visible(), (
            "a page that has not loaded the account's records painted its empty "
            "state as if the account had no muted words"
        )
        assert app.muted_words.words() == [], (
            f"a never-listed replica painted rows {app.muted_words.words()}"
        )

        app.muted_words.add(refused_word)
        wait_until(
            says_it_needs_the_nest, REFUSAL_S, interval=1.0,
            diagnose=lambda: (
                f"the add on a never-listed replica left error-message "
                f"{app.error_text()!r}; the refusal must say {S.common.needs_nest!r}"
            ),
        )
        assert refused_word not in app.muted_words.words(), (
            f"an edit on a never-listed replica was applied: rows "
            f"{app.muted_words.words()}"
        )
    finally:
        start_nest_in_place(nest_instance)

    # The nest is back: the seat lists the account, and what the account already
    # held is there, while the refused edit never happened.
    seen: dict[str, list[str]] = {}

    def listed() -> bool:
        poked_pass(app.driver, what="the first listing")
        seen["rows"] = app.muted_words.revisit()
        return kept_word in seen["rows"]

    wait_until(
        listed, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"after the nest came back the seat lists {seen.get('rows')}; the "
            f"account's {kept_word!r} must be there. error-message "
            f"{app.error_text()!r}"
        ),
    )
    assert refused_word not in seen["rows"], (
        f"the refused edit {refused_word!r} surfaced after the first listing: "
        f"rows {seen['rows']} — it was queued, not refused"
    )
