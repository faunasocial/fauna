"""The private contact overlay, end to end on every app that has built it.

Goal docs: `docs/goal/ui/contacts.md` § The private overlay (the record, the
per-field merge, where the nickname paints and its two guards, labels in the
roster filter) and `docs/goal/ui/profile.md` § The private section (the edit
surface). The overlay is one `fauna.state.contact-overlay` account-plane item
per person, fleet-only and GenerationTip-sealed
(`docs/goal/architecture/account-data-taxonomy.md` → *The contact-overlay
rung*), so a Save in a fresh session is also the first-need mint of this
device's generation tip: the harness seeds escrow trust on every launch, and
a refused Save would surface through the Profile's `error-message` — which is
why every wait below reads it into its failure (convention 6).

tier_3: the real nest, the real app binary, the real account runtime. tui
led (the Profile private section and the roster row were tui's first), linux,
android, macos and ios followed; each other app joins the marker list below with its own leg. The
two-seat test's second seat is always a tui seat (`helpers.fleet.SiblingSeats`),
so on another app it needs tui in the run's app set (`--app linux,tui`).

Every mutation is a UI gesture (convention 8); the contact edge the Profile is
reached through is API arrangement, as in `test_profile.py`. Every wait is a
deadline poll on painted state (convention 14).
"""
from __future__ import annotations

import time

import pytest

from helpers.fleet import (
    FLEET_MEMBER_VISIBLE_S,
    SiblingSeats,
    await_fleet_member,
    fleet_id_hex,
    poked_pass,
)
from helpers.other_profile import open_other_profile, seed_accepted_contact
from helpers.waiting import await_account_runtime_assembled, wait_until

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.android,
    pytest.mark.macos, pytest.mark.ios,
]

#: A Save in a fresh session first-need mints the generation tip (an escrow
#: deposit + a publish) before its local write lands — the trust-seed
#: witness's own budget for a fresh actor's first mint.
FIRST_SAVE_S = 240.0

#: A painted-state re-read after a local gesture or a relaunch.
PAINT_S = 30.0

#: A sibling seat hearing of the other seat's write: its publish, the nest's
#: relay, the other seat's walk — pump passes, poked where this process holds
#: the pump.
CONVERGE_S = 240.0


def _nonce() -> str:
    return f"{int(time.time() * 1000) % 1_000_000:06d}"


def _save_and_await_header(app, nickname: str, *, where: str) -> None:
    """Save the staged private section and wait for the nickname to head the
    Profile — the Save's effect, and the first-need mint's."""
    app.profile.save_private()
    wait_until(
        lambda: app.profile.handle_text() == nickname,
        FIRST_SAVE_S,
        diagnose=lambda: (
            f"{where}: the Profile header never became the saved nickname "
            f"{nickname!r} (header {app.profile.handle_text()!r}, "
            f"error-message {app.profile.error_text()!r}) — a no-tip refusal "
            f"names itself there"
        ),
    )


def _await_row(app, other_hex: str, expected, *, where: str, budget_s: float = PAINT_S,
               between=None) -> None:
    """Narrow the roster to ``other_hex`` and poll row 0's three names until
    they match ``expected`` (a tuple, or a predicate over the tuple)."""
    app.contacts.navigate()
    app.contacts.narrow_roster(other_hex)
    last: list = []
    matches = expected if callable(expected) else (lambda names: names == expected)

    def ok():
        if between is not None:
            between()
        if app.contacts.contact_count() != 1:
            last[:] = [f"{app.contacts.contact_count()} rows"]
            return False
        names = app.contacts.row_names(0)
        last[:] = [names]
        return matches(names)

    wait_until(
        ok, budget_s, interval=1.0,
        diagnose=lambda: f"{where}: roster row for {other_hex[:16]}… read {last}, wanted {expected}",
    )


@pytest.mark.feature("profile")
@pytest.mark.feature("contacts")
def test_a_nickname_notes_and_label_paint_on_the_roster_and_profile_and_survive_a_relaunch(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    other = seed_accepted_contact(nest_instance, test_user)
    nonce = _nonce()
    nickname = f"Mum {nonce}"
    label = f"Book club {nonce}"

    # The row before: its public name, no secondary lines.
    app.contacts.navigate()
    app.contacts.narrow_roster(other)
    wait_until(lambda: app.contacts.contact_count() == 1, PAINT_S,
               diagnose=lambda: f"the seeded contact never listed ({app.contacts.contact_names()})")
    public, before_public_line, before_labels = app.contacts.row_names(0)
    assert public and not before_public_line and not before_labels, (
        f"an overlay-free row shows its public name only, got {(public, before_public_line, before_labels)}"
    )

    open_other_profile(app, other)
    header_public = app.profile.handle_text()
    assert app.profile.public_name_text() == "", "no nickname yet: no secondary line"

    app.profile.set_private_nickname(nickname)
    app.profile.set_private_notes("likes tea")
    app.profile.add_private_label(label)
    assert app.profile.private_labels() == [label], "the label is staged as a chip"
    _save_and_await_header(app, nickname, where="the first Save")
    assert app.profile.public_name_text() == header_public, (
        "guard 1: the public name the nickname replaced stays on the header"
    )

    # The roster row: the nickname heads it, the public name beneath, the label.
    _await_row(app, other, (nickname, public, label), where="after the Save")

    # Labels reach the roster filter: typing the label narrows to this person.
    app.contacts.narrow_roster(label.lower())
    wait_until(
        lambda: app.contacts.contact_names() == [nickname], PAINT_S,
        diagnose=lambda: f"the label filter showed {app.contacts.contact_names()}",
    )

    # A relaunch: the overlay is account state, not the old process's memory.
    app.driver.hard_reload()
    await_account_runtime_assembled(app.driver)
    _await_row(app, other, (nickname, public, label), where="after the relaunch")
    open_other_profile(app, other)
    wait_until(
        lambda: (app.profile.handle_text(), app.profile.private_notes()) == (nickname, "likes tea"),
        PAINT_S,
        diagnose=lambda: (
            f"after the relaunch the Profile read header {app.profile.handle_text()!r}, "
            f"notes {app.profile.private_notes()!r}"
        ),
    )


@pytest.fixture
def sibling_seats(request, nest_instance):
    """Second enrolled seats on the session nest (`helpers.fleet.SiblingSeats`)."""
    seats = SiblingSeats(request, nest_instance)
    try:
        yield seats
    finally:
        seats.teardown()


@pytest.mark.feature("profile")
def test_two_seats_of_one_account_converge_on_the_overlay_per_field(
    app, request, nest_instance, sibling_seats
):
    """Convention 16: two seats of one account on this machine. A nickname
    saved on seat A paints on seat B's roster with no relaunch; notes saved on
    seat B reach seat A's Profile — and neither write clobbers the other's
    field (`contacts.md` § The private overlay: per-register merge)."""
    from conftest import _make_user
    from helpers import enrollment

    url = nest_instance["url"]
    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, url, user)
    seat_b = sibling_seats.launch(user=user)
    writer_b, _, _ = enrollment.await_enrollment(seat_b, url, user)
    # Seat A's first Save mints the generation over the fleet it has merged,
    # so seat B must be a verified member of it first, or B could never open
    # the tip-sealed row A writes.
    await_fleet_member(app, seat_b, fleet_id_hex(writer_b), budget_s=FLEET_MEMBER_VISIBLE_S)

    other = seed_accepted_contact(nest_instance, user)
    nickname = f"Aunt {_nonce()}"

    def both_passes():
        poked_pass(app.driver, what="seat A's publish")
        poked_pass(seat_b.driver, what="seat B's walk")

    open_other_profile(app, other)
    app.profile.set_private_nickname(nickname)
    _save_and_await_header(app, nickname, where="seat A's Save")

    _await_row(
        seat_b, other, lambda names: names[0] == nickname,
        where="seat B, after seat A saved the nickname (no relaunch)",
        budget_s=CONVERGE_S, between=both_passes,
    )

    open_other_profile(seat_b, other)
    seat_b.profile.set_private_notes("moved house")
    seat_b.profile.save_private()
    wait_until(
        lambda: seat_b.profile.private_notes() == "moved house"
        and seat_b.profile.error_text() == "",
        PAINT_S,
        diagnose=lambda: f"seat B's Save: error-message {seat_b.profile.error_text()!r}",
    )

    def a_converged():
        both_passes()
        return (app.profile.private_notes(), app.profile.handle_text()) == ("moved house", nickname)

    wait_until(
        a_converged, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"seat A's Profile read notes {app.profile.private_notes()!r}, header "
            f"{app.profile.handle_text()!r}: B's notes should have arrived and A's "
            f"nickname survived"
        ),
    )
    assert seat_b.profile.handle_text() == nickname, "seat B's notes Save kept A's nickname"
