"""Signed-in devices without a matching entry — the Devices page's member group
(``device-member-card``), and the member-addressed removal it offers.

Owner: ``docs/goal/ui/devices.md`` § Members without a matching entry (the
surface) and ``docs/goal/architecture/account-data-taxonomy.md`` § The
generation machinery → *Fleet-scope reclamation*, clause (4), *A disagreement is
the user's to settle* (the rule). The group lists every verified fleet member —
a device holding this account's keys — that no roster row accounts for, by its
key fingerprint, and removes it BY THAT KEY: no nest input (a hostile nest
cannot aim it), no row statement read (the member cannot veto it). Shared Rust
end to end (``fauna_core::fleet_removal::unaccounted_members`` →
``AccountStoreHandle::unaccounted_fleet_members`` → ``DevicesMachine``), driven
here through the app UI (convention 8).

**Two journeys, two of the group's members.**

1. *A device that mis-states its own row* — the stolen laptop running modified
   code. The sibling seat publishes a fabricated
   ``enrolled_row`` on its own sealed device-endpoints entry, which only a
   member can write and the harness cannot forge from outside: the e2e-only
   ``FAUNA_E2E_STATED_ROW`` override in the account runtime's pump (compiled out
   of release builds, convention 15) is the one door. Removing it BY ITS ROW is
   refused (``devices.error_remove_row_mismatch`` — a nest re-pairing the row
   and a member lying about it present identical facts, so the row gesture
   trusts neither); its card lists it; the card removes it; and the row gesture
   then finishes the nest row, whose claim now names a removed id.

2. *A device whose entry was deleted elsewhere* — a nest row deleted with no
   fleet leg beside it (catalog outcome 10), as every web removal was before
   web's Devices page reached its runtime through the account port. The
   fixture deletes the sibling's row over the API as the user (that exact
   effect); the member stays verified, its card lists it by fingerprint, and
   the card removes it.

**Two seats, one account.** The sibling is a real second ``fauna-tui`` seat
(`helpers.fleet.SiblingSeats`), never a seeded roster row: only an app with its
own account runtime publishes the ``device-set`` enrolment and the row
statement the derivation reads. It is stopped before any gesture — a live seat
keeps re-stating its row, and a removed one would re-mint a successor principal
and list a NEW card under this journey's assertions.

⚠ Every wait is a deadline poll on state (convention 14): the enrolment latch,
the plane merge (`await_fleet_member`), the member card appearing on a nav-edge
re-hydrate (that IS the "statement merged" barrier — before the statement
lands, the sibling's row still resolves to it alone and nothing is listed),
the refusal on ``error-message`` polled WITHOUT re-navigating (a hydrate clears
the page error), and the ``Removed`` row read back off the remover's own plane
through ``device_set_state``. The fingerprint the card must show is computed
here from the sibling's writer key — an oracle independent of the app's
formatter, which its own Rust pins.

⚠ A DEDICATED actor, never the shared ``test_user`` — which is still requested:
its fixture lifts the device cap on the tier every test user shares, and this
journey needs two enrolled seats.
"""
from __future__ import annotations

import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import enrollment
from helpers.app_surface import app_name, skip_unbuilt
from helpers.fleet import (
    FLEET_MEMBER_VISIBLE_S,
    SiblingSeats,
    await_fleet_member,
    device_set_state,
    fleet_id_hex,
    poked_pass,
    require_device_set_reader,
)
from helpers.waiting import await_device_removal_ready, wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: The member card appearing at seat one: the sibling's statement published and
#: merged (a poked pass on each side per poll), plus a nav-edge hydrate reading
#: the door. Generous by design; a green run pays only the real latency.
MEMBER_CARD_VISIBLE_S = 240.0

#: A refusal is local (the runtime resolves against its own replica) — one
#: gesture plus one repaint.
REFUSAL_VISIBLE_S = 30.0

#: The card's confirm writing the `Removed` row on this seat's own replica —
#: a local write, read back through the runtime.
REMOVED_VISIBLE_S = 60.0

#: The nest reflecting a row deletion after the follow-up row gesture.
ROSTER_DELETION_VISIBLE_S = 60.0

#: How often a poll re-enters the Devices page so tui's nav-edge hydrate
#: re-reads the roster and the door — a re-hydrate cadence, not a settle.
REHYDRATE_EVERY_S = 2.0

#: A row id no nest ever minted — what the mis-stating seat claims as its own.
FABRICATED_ROW = "f" * 64


@pytest.fixture
def sibling_seats(request, nest_instance):
    """Second enrolled seats on the session nest (`helpers.fleet.SiblingSeats`)."""
    seats = SiblingSeats(request, nest_instance)
    try:
        yield seats
    finally:
        seats.teardown()


def _fingerprint_text(fleet_id: str) -> str:
    """What a card shows for `fleet_id`: `devices.member_fingerprint` over the
    shared `fleet_fingerprint` — first 8 + `…` + last 8 hex. Computed here, not
    read back, so the assertion is an oracle on the app's rendering."""
    return S.devices.member_fingerprint(fingerprint=f"{fleet_id[:8]}…{fleet_id[-8:]}")


def _own_fingerprint_text(fleet_id: str) -> str:
    return S.devices.own_fingerprint(fingerprint=f"{fleet_id[:8]}…{fleet_id[-8:]}")


def _member_fingerprints(driver) -> list[str]:
    return [
        driver.get_text("device-member-fingerprint", scope=f"device-member-card[{i}]") or ""
        for i in range(driver.count("device-member-card"))
    ]


def _error_text(driver) -> str:
    if not driver.is_visible("error-message"):
        return ""
    return driver.get_text("error-message") or ""


def _require_member_group(driver) -> None:
    """Convention 7: built on tui (lead app, 2026-09-25), macOS + iOS (shared
    FaunaKit, trickle-down leg, 2026-09-25) and web (2026-09-30, its Devices
    machine reaching the runtime through the account port);
    linux/windows/android still owe their legs."""
    if app_name(driver) not in ("tui", "macos", "ios", "web"):
        skip_unbuilt(
            driver,
            surface="device-member-card (signed-in devices without a matching entry)",
            detail=(
                "the member group and device-own-fingerprint are built on tui, "
                "macOS, iOS and web; this app's leg renders DevicesSnapshot.members / "
                "own_fingerprint from the shared machine (no re-derivation owed)"
            ),
            tracked="docs/goal/ui/devices.md § Implementation status today",
        )


def _delete_row_as_the_user(nest_url: str, user: dict, row: str) -> None:
    """A client that ran no fleet leg: `fauna.sync.devices.delete` as the user,
    nothing else, so no `Removed` row. Fixture arrangement (the device removed
    ELSEWHERE — no app's own row gesture does this any more), never the action
    under test."""
    actor = bytes.fromhex(user["actor_id_hex"])
    with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
        client.call("fauna.sync.devices.delete", {"device_id": row})


def _await_member_card(app, sibling, target: str) -> int:
    """Barrier: a `device-member-card` for `target` is painted at seat one.
    Returns its index. Each poll pokes both pumps (the sibling has to PUBLISH,
    seat one has to MERGE) and re-enters the page so the nav-edge hydrate
    re-reads the door. `sibling` may be `None` once it is stopped."""
    driver = app.driver
    expected = _fingerprint_text(target)
    last: dict = {}
    last_nav = [0.0]

    def listed():
        if sibling is not None:
            poked_pass(sibling.driver, what="the sibling's plane publish")
        poked_pass(driver, what="seat one's fleet-view merge")
        if time.monotonic() - last_nav[0] >= REHYDRATE_EVERY_S:
            app.backups.navigate_devices()
            last_nav[0] = time.monotonic()
        last["cards"] = _member_fingerprints(driver)
        last["error"] = _error_text(driver)
        return expected in last["cards"]

    wait_until(
        listed,
        MEMBER_CARD_VISIBLE_S,
        interval=1.0,
        diagnose=lambda: (
            f"no device-member-card ever showed {expected!r} (last cards: "
            f"{last.get('cards')!r}, error-message: {last.get('error')!r}). The "
            f"sibling is a verified member here (device_set_state said enrolled), "
            f"so either its row STATEMENT never merged (the group lists it only "
            f"once no roster row resolves to it alone) or the page reads no "
            f"member door — an app without the leg paints no cards at all. "
            f"{driver.diagnose('device-member-card')}"
        ),
    )
    return last["cards"].index(expected)


def _remove_by_card(app, index: int, target: str, me: str) -> None:
    """The gesture under test (convention 8): the card's two-step remove, then
    the `Removed` row read back off this seat's own replica — attributed to
    this device, the one that removed it."""
    driver = app.driver
    driver.click("device-member-remove-button", index=index)
    driver.wait_for(
        "device-member-remove-confirm-button", timeout=10.0, scope=f"device-member-card[{index}]"
    )
    assert driver.count("device-member-remove-confirm-button") == 1, (
        "the confirm pair arms on exactly the clicked card: "
        f"{driver.diagnose('device-member-remove-confirm-button')}"
    )
    driver.click("device-member-remove-confirm-button", scope=f"device-member-card[{index}]")

    last: dict = {}

    def removed():
        last.clear()
        last.update(device_set_state(driver, target))
        last["error"] = _error_text(driver)
        return last.get("found") and last.get("state") == "removed"

    wait_until(
        removed,
        REMOVED_VISIBLE_S,
        interval=0.5,
        diagnose=lambda: (
            f"the card's confirm never wrote the sibling's Removed row on this "
            f"seat's replica (last device-set read: {last!r}). A refusal on "
            f"error-message names why; an empty error with an enrolled row = the "
            f"member door's leg did not run (grep the app log for 'remove_member')."
        ),
    )
    assert last.get("removed_by") == me, (
        f"the Removed row is attributed to the device that removed it, {me[:16]}…, "
        f"not {last.get('removed_by')!r}"
    )


def _arrange_two_seats(app, request, nest_instance, sibling_seats, *, sibling_env=None):
    """Seat one signed in on a dedicated actor and enrolled; a second seat
    enrolled on the same account and merged into seat one's fleet view; the
    sibling then STOPPED. Returns `(user, me, row_one, row_two, target)`."""
    from conftest import _make_user

    driver = app.driver
    url = nest_instance["url"]
    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    _require_member_group(driver)
    require_device_set_reader(driver)

    writer_one, row_one, _ = enrollment.await_enrollment(app, url, user)
    me = fleet_id_hex(writer_one)
    sibling = sibling_seats.launch(user, environment=sibling_env)
    writer_two, row_two, _ = enrollment.await_enrollment(sibling, url, user)
    target = fleet_id_hex(writer_two)
    assert row_one != row_two and me != target, (
        f"the two seats did not enrol as distinct devices (rows {row_one!r} vs "
        f"{row_two!r}, fleet ids {me[:16]}… vs {target[:16]}…)"
    )
    await_fleet_member(app, sibling, target, budget_s=FLEET_MEMBER_VISIBLE_S)
    return user, me, row_one, row_two, target, sibling


@pytest.mark.feature("devices")
def test_a_device_that_misstates_its_row_is_refused_on_its_row_and_removed_by_its_card(
    app, request, nest_instance, test_user, sibling_seats
):
    """The stolen device: its statement names a fabricated row. By its row the
    removal is refused with the row-mismatch copy and nothing is deleted; its
    card lists it by fingerprint; the card removes it by key; and the row
    gesture then takes the nest row off, since its claim now names a removed
    id."""
    driver = app.driver
    url = nest_instance["url"]
    user, me, row_one, row_two, target, sibling = _arrange_two_seats(
        app, request, nest_instance, sibling_seats,
        sibling_env={"FAUNA_E2E_STATED_ROW": FABRICATED_ROW},
    )

    # The statement is what lists the card: before it merges, the sibling's row
    # still resolves to the sibling alone and the group is empty. So the card
    # IS the barrier — and the sibling stays up until it is seen, since it is
    # the only writer of that statement.
    card = _await_member_card(app, sibling, target)
    sibling.driver.teardown()

    # --- by its row: refused, nothing deleted.
    listed = list(enrollment.roster(url, user))
    assert row_two in listed, (listed, row_two)
    index_two = listed.index(row_two)
    assert app.backups.device_count() == len(listed), driver.diagnose("device-card")
    await_device_removal_ready(driver)
    driver.click("device-remove-button", index=index_two)
    last: dict = {}

    def refused():
        last["error"] = _error_text(driver)
        return last["error"] == S.devices.error_remove_row_mismatch

    wait_until(
        refused,
        REFUSAL_VISIBLE_S,
        interval=0.5,
        diagnose=lambda: (
            f"removing the mis-stating sibling by its ROW never painted the "
            f"row-mismatch refusal (last error-message: {last.get('error')!r}). An "
            f"empty error with the row gone = the statement was not consulted."
        ),
    )
    assert row_two in enrollment.roster(url, user), "a refused removal deletes nothing"
    state = device_set_state(driver, target)
    assert state.get("state") == "enrolled", ("a refused removal writes nothing", state)
    assert "without a matching entry" in S.devices.error_remove_row_mismatch, (
        "the refusal copy points the user at the member group"
    )

    # --- by its card: removed by key. The card is still where it was — the
    # refusal repainted nothing (no refresh on a refusal).
    _remove_by_card(app, card, target, me)

    # The group empties on the next hydrate (the member is now excluded), and
    # the nest row is still there: the member door touches no nest row.
    app.backups.navigate_devices()
    driver.wait_for("device-card", timeout=30.0)
    assert _fingerprint_text(target) not in _member_fingerprints(driver), (
        driver.diagnose("device-member-card")
    )
    assert row_two in enrollment.roster(url, user), (
        "the member door leaves the nest row for the row gesture"
    )

    # --- the row gesture now finishes it: the claim names a removed id, so it
    # resolves to nothing and the nest deletion proceeds alone.
    listed = list(enrollment.roster(url, user))
    index_two = listed.index(row_two)
    assert app.backups.device_count() == len(listed), driver.diagnose("device-card")
    driver.click("device-remove-button", index=index_two)
    gone: dict = {}

    def row_gone():
        gone["roster"] = list(enrollment.roster(url, user))
        gone["error"] = _error_text(driver)
        return row_two not in gone["roster"]

    wait_until(
        row_gone,
        ROSTER_DELETION_VISIBLE_S,
        interval=1.0,
        diagnose=lambda: (
            f"the row gesture after the member removal never deleted the nest row "
            f"(roster {gone.get('roster')!r}, error-message {gone.get('error')!r}): a "
            f"claim naming a removed id must resolve to nothing, not refuse."
        ),
    )


@pytest.mark.feature("devices")
def test_a_device_whose_entry_was_deleted_elsewhere_is_listed_by_fingerprint_and_removed(
    app, request, nest_instance, test_user, sibling_seats
):
    """Catalog outcome 10: a device the fleet still counts but no row here can
    name — its nest row deleted by a client that ran no fleet leg — shows by its fingerprint,
    beside this device's own on its own row, and is removed by its card."""
    driver = app.driver
    url = nest_instance["url"]
    user, me, row_one, row_two, target, sibling = _arrange_two_seats(
        app, request, nest_instance, sibling_seats
    )
    # An honest sibling: with its row on the roster, nothing is listed — the
    # group appears only once the row is gone. Read on a fresh hydrate.
    app.backups.navigate_devices()
    driver.wait_for("device-card", timeout=30.0)
    assert driver.count("device-member-card") == 0, (
        f"an honest fleet lists nobody: {driver.diagnose('device-member-card')}"
    )
    sibling.driver.teardown()

    # A client that ran no fleet leg: the nest row alone, deleted as the user.
    _delete_row_as_the_user(url, user, row_two)
    assert row_two not in enrollment.roster(url, user)

    card = _await_member_card(app, None, target)
    listed = list(enrollment.roster(url, user))
    assert app.backups.device_count() == len(listed), (
        f"the roster paints the nest's rows, the deleted one gone: "
        f"{driver.diagnose('device-card')}"
    )

    # This device's own half of the comparison, on its own (marked) row.
    marked = [
        i for i in range(driver.count("device-card"))
        if driver.is_visible_scrolled("device-this-mark-badge", scope=f"device-card[{i}]")
    ]
    assert len(marked) == 1, driver.diagnose("device-this-mark-badge")
    assert driver.get_text("device-own-fingerprint", scope=f"device-card[{marked[0]}]") == (
        _own_fingerprint_text(me)
    ), driver.diagnose("device-own-fingerprint")
    assert driver.count("device-own-fingerprint") == 1, "only this device's row carries it"
    assert driver.is_visible("device-member-note"), driver.diagnose("device-member-note")

    _remove_by_card(app, card, target, me)

    app.backups.navigate_devices()
    driver.wait_for("device-card", timeout=30.0)
    assert driver.count("device-member-card") == 0, (
        f"a removed member is no wrap target, so its card is gone: "
        f"{driver.diagnose('device-member-card')}"
    )
