"""Muted words are one list across a person's devices, and a word added on one
device is never lost because another device changed the list at the same time.

Goal doc: ``docs/goal/architecture/content-moderation-and-ranking.md``
§ Implementation status today, leg 4 (the muted list rides the account's
sealed ``UserConfig.moderation`` and merges per entry, so two concurrent adds
both survive).

The journey: two seats of one account on this machine (convention 16) — seat A
the app under test, seat B the fleet's tui sibling (`helpers.fleet.SiblingSeats`).
Each seat adds its own word through its own Muted words page, back to back,
before either has seen the other's; both words must then be listed on BOTH
seats. A last-writer-wins list would leave one seat's word missing everywhere.

tier_3: the real nest, two real seats, the real account runtimes. Every
mutation is a UI gesture (convention 8); every wait is a deadline poll on
painted state, with the pump poked where the seat holds it (convention 14).
Each poll re-visits the page, so it holds whether or not the app consumes the
store-change notice (`test_store_change_notice.py` owns that promise).
"""
from __future__ import annotations

import time

import pytest

from helpers.fleet import SiblingSeats, poked_pass
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

#: A seat's own add round trip.
ADD_S = 120.0

#: Each seat's publish, the nest's relay, the other seat's walk.
CONVERGE_S = 240.0


@pytest.fixture
def sibling_seats(request, nest_instance):
    """Second enrolled seats on the session nest (`helpers.fleet.SiblingSeats`)."""
    seats = SiblingSeats(request, nest_instance)
    try:
        yield seats
    finally:
        seats.teardown()


@pytest.mark.feature("muted-words")
def test_words_added_on_two_devices_at_once_are_both_kept_on_both(
    app, request, nest_instance, sibling_seats
):
    from conftest import _make_user
    from helpers import enrollment

    url = nest_instance["url"]
    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, url, user)
    seat_b = sibling_seats.launch(user=user)
    enrollment.await_enrollment(seat_b, url, user)

    stamp = int(time.time() * 1000) % 1_000_000
    word_a, word_b = f"seata{stamp:06d}", f"seatb{stamp:06d}"

    app.muted_words.navigate()
    seat_b.muted_words.navigate()
    # Back to back: neither seat has walked the other's change when it writes.
    app.muted_words.add(word_a)
    seat_b.muted_words.add(word_b)
    assert app.muted_words.wait_for_word(word_a, timeout=ADD_S), (
        f"seat A's add never listed {word_a!r} (rows {app.muted_words.words()}, "
        f"error-message {app.error_text()!r})"
    )
    assert seat_b.muted_words.wait_for_word(word_b, timeout=ADD_S), (
        f"seat B's add never listed {word_b!r} (rows {seat_b.muted_words.words()}, "
        f"error-message {seat_b.error_text()!r})"
    )

    seen: dict[str, list[str]] = {}

    def both_hold_both() -> bool:
        poked_pass(app.driver, what="seat A's walk")
        poked_pass(seat_b.driver, what="seat B's walk")
        seen["a"] = app.muted_words.revisit()
        seen["b"] = seat_b.muted_words.revisit()
        return all({word_a, word_b} <= set(words) for words in seen.values())

    wait_until(
        both_hold_both, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"after two concurrent adds the seats list A={seen.get('a')} "
            f"B={seen.get('b')}; both must hold {word_a!r} and {word_b!r} — a "
            f"word missing on both seats is a lost write, one missing on one "
            f"seat is a walk that never converged; error-message "
            f"A={app.error_text()!r} B={seat_b.error_text()!r}"
        ),
    )
