"""The store-change notice: an OPEN store-backed page repaints, whoever changed
the store.

Goal doc: ``docs/goal/architecture/account-runtime.md`` § Multi-instance
concurrency → *A runtime's own pump is a source of the notice too*: an
open surface whose render source is read through the account store shows what
a fresh visit would show, within the notice's latency, whichever process
applied the change. "Read once per visit" is not a contract.

The journey: two seats of one account on this machine (convention 16). Seat A
is the app under test (tui, linux, web); seat B is the fleet's tui sibling
(`helpers.fleet.SiblingSeats`), so the linux leg runs under `--app linux,tui`
and the web leg under `--app web,tui`.
Seat A opens Muted words and stays there; seat B adds a word through its own page.
The word must appear on seat A's open page with no re-visit. Seat A's change
arrives one of two ways, and the ruling makes them one rule: when A's runtime
holds the pump, its own walk applies B's row and its change generation is the
notice; when a co-located process holds it, that process's commit moves the
store's ``data_version`` floor. The failure message names which role A held.
A web tab always holds its own pump (no agent sits beside a browser, and
IndexedDB has no floor), so the web leg is the own-pump source alone.

tier_3: the real nest, two real seats, the real account runtimes. Every
mutation is a UI gesture (convention 8); every wait is a deadline poll on
painted state, with the pump poked where the seat holds it (convention 14).
"""
from __future__ import annotations

import time

import pytest

from helpers.fleet import SiblingSeats, poked_pass
from helpers.waiting import account_pump_role, wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.web]

#: The seat's own write: its page's add round trip.
ADD_S = 120.0

#: Seat B's publish, the nest's relay, seat A's walk, then the notice and the
#: open page's reload — pump passes, poked where the seat holds the pump; the
#: floor's own cadence (10 s) where it does not.
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
def test_an_open_muted_words_page_shows_another_devices_word_without_a_revisit(
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

    word = f"notice{int(time.time() * 1000) % 1_000_000:06d}"

    # Seat A opens the page ONCE and never leaves it.
    app.muted_words.navigate()
    assert app.muted_words.wait_for_empty_state(), (
        f"seat A's fresh account should read an empty list "
        f"(rows {app.muted_words.words()}, error-message {app.error_text()!r})"
    )

    seat_b.muted_words.navigate()
    seat_b.muted_words.add(word)
    assert seat_b.muted_words.wait_for_word(word, timeout=ADD_S), (
        f"seat B's add never listed {word!r} (rows {seat_b.muted_words.words()}, "
        f"error-message {seat_b.error_text()!r})"
    )

    def a_shows_it():
        poked_pass(seat_b.driver, what="seat B's publish")
        poked_pass(app.driver, what="seat A's walk")
        try:
            return word in app.muted_words.words()
        except LookupError:
            return False  # a row the list rebuilt away between the count and the read

    wait_until(
        a_shows_it, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"seat A's OPEN page still lists {app.muted_words.words()} — {word!r} never "
            f"painted without a re-visit; seat A's pump role (runtime, holder) = "
            f"{account_pump_role(app.driver)}, seat B's = {account_pump_role(seat_b.driver)}; "
            f"a re-visit now reads {app.muted_words.revisit()} (if it lists the word, the "
            f"change landed and the notice never re-drove the open page); "
            f"error-message {app.error_text()!r}"
        ),
    )
