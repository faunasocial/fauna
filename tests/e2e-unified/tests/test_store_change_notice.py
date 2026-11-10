"""The store-change notice: an OPEN store-backed page repaints, whoever changed
the store.

Goal doc: ``docs/goal/architecture/account-runtime.md`` § Multi-instance
concurrency → *A runtime's own pump is a source of the notice too*: an
open surface whose render source is read through the account store shows what
a fresh visit would show, within the notice's latency, whichever process
applied the change. "Read once per visit" is not a contract.

The journey: two seats of one account on this machine (convention 16). Seat A
is the app under test (tui, linux, web, macos, ios, windows); seat B is the fleet's tui
sibling (`helpers.fleet.SiblingSeats`), so the linux leg runs under
`--app linux,tui`, the web leg under `--app web,tui`, the android leg under
`--app android,tui`, the Apple legs under `--app macos,tui` / `--app ios,tui`
and the windows leg (the muted-words case) under `--app windows,tui`.
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

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
]

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


@pytest.mark.windows
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


@pytest.mark.feature("backup-destinations-and-restore")
def test_an_open_backups_page_shows_a_destination_another_device_adds_and_renames(
    app, request, nest_instance, second_nest, sibling_seats
):
    """**A destination added or changed on another of your devices shows on an
    open Backups page without leaving it** (`backup-destinations-and-restore`
    outcome 31; the rule is this module's: an open store-backed page repaints,
    whoever changed the store).

    A destination row lives on the account plane (`fauna.state.backup`), so seat
    B's add is a store write seat A's pump applies, and the notice re-drives
    seat A's open page (`Page::Backups`'s store resync on tui). Seat A opens
    Backups once and never leaves; seat B enrolls ``second_nest`` through its own
    page, then renames it. Each must paint on seat A with no re-visit.
    """
    from common.auth import register_user
    from conftest import _make_user
    from helpers import enrollment
    from helpers.backup_destinations import DestinationEnrollment

    url = nest_instance["url"]
    user = _make_user(nest_instance)
    # A v1 destination is a nest the owner administers: the enroll's handshake
    # succeeds only for an identity registered there (fixture setup).
    register_user(
        second_nest["port"], user["actor_id_hex"],
        admin_signing_key=second_nest["admin"]["signing_key"],
    )
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, url, user)
    app.backups.require_destination_management_supported()
    seat_b = sibling_seats.launch(user=user)
    enrollment.await_enrollment(seat_b, url, user)

    # Seat A opens the page ONCE and never leaves it.
    app.backups.navigate()

    def a_lists(name: str):
        def check():
            poked_pass(seat_b.driver, what="seat B's publish")
            poked_pass(app.driver, what="seat A's walk")
            try:
                return any(name in app.backups.destination_text(i)
                           for i in range(app.backups.destination_count()))
            except LookupError:
                return False  # a row the list rebuilt away between the count and the read
        return check

    def a_rows():
        try:
            return [app.backups.destination_text(i)
                    for i in range(app.backups.destination_count())]
        except LookupError:
            return "<rebuilding>"

    dest = DestinationEnrollment(seat_b, nest_instance, user)
    request.addfinalizer(dest.leave_none)
    added = dest.add_destination(second_nest["url"], "Sibling-added")
    wait_until(
        a_lists(added), CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"seat A's OPEN Backups page lists {a_rows()!r} — seat B's destination "
            f"{added!r} never painted without a re-visit; seat A's pump role "
            f"(runtime, holder) = {account_pump_role(app.driver)}, seat B's = "
            f"{account_pump_role(seat_b.driver)}; error-message {app.error_text()!r}"
        ),
    )

    # A change, not only an add: the rename keeps the enrolled name as its
    # prefix so the finalizer still knows the row as this test's.
    renamed = f"{added}-renamed"
    seat_b.backups.edit_destination(0, name=renamed)
    wait_until(
        lambda: renamed in seat_b.backups.destination_text(0), ADD_S,
        diagnose=lambda: f"seat B's own rename never painted: "
                         f"{seat_b.backups.destination_text(0)!r} "
                         f"error-message {seat_b.error_text()!r}",
    )
    wait_until(
        a_lists(renamed), CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"seat A's OPEN Backups page lists {a_rows()!r} — seat B's rename to "
            f"{renamed!r} never painted without a re-visit; seat A's pump role = "
            f"{account_pump_role(app.driver)}; error-message {app.error_text()!r}"
        ),
    )
