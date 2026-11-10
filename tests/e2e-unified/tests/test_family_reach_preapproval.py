"""family-safety outcome 60 — a guardian approves a contact for the child ahead
of any request, and a child under contact approval can read a held knock but
cannot dismiss it.

`docs/goal/behavior/family-safety.md` § Reach approvals and § Child-initiated
contact requests: `fauna.family.contact.add` is the guardian's out-of-band
pre-approval ("can I talk to X?" across the dinner table) — it mints the
accepted edge the child's own accept would, and a matching pending knock is
dismissed with its held arrival delivered. § Implementation status today (the
reach pillar paragraph): `fauna.knocks.{unblock,dismiss}` are refused for a ward
under `contact_approval` — reads stay open — because a held knock is the
guardian's to decide, and a child relocating it out from under the queue would
erode that authority.

The mutations under test go through the app (convention 8): the guardian's
editor toggle and save, the guardian's `family-contact-add` gesture, and the
child's own `knock-dismiss` click. The strangers' knocks are stored through the
nest's knock door (the strangers are not the users under test), and the
child's contact edges are read back over the wire (convention 5).

tui offers no unblock gesture on a knock row, so the unblock half of the nest
refusal has no app surface to witness here; it is pinned at the nest
conformance tier.
"""

import pytest

from helpers.admin_wire import admit_adult
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until
from tests.test_family import (  # noqa: F401
    _contact_status,
    _guardian_opens_editor,
    _push_knock,
    _shared_family,
    _unique_suffix,
    family_pair,
)

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("family-safety")
def test_family_guardian_pre_approves_a_contact_and_the_child_cannot_dismiss_a_held_knock(
    admin_app, request, nest_instance, family_pair
):
    from conftest import _login_app_as

    app = admin_app
    url = nest_instance["url"]
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair

    # ── The guardian turns contact approval on. ──
    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_contact_approval(True)
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"

    suffix = _unique_suffix()
    knocker = admit_adult(nest_instance, f"family-held-knock-{suffix}")
    friend = admit_adult(nest_instance, f"family-preapproved-{suffix}")
    knock_text = f"may I talk to you {suffix}"
    _push_knock(url, ward_identity["actor_id_hex"], knocker["actor_id_hex"], knock_text)

    # ── The child reads the held knock, and its dismiss is refused. ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    app.contacts.navigate()

    def _knock_row():
        row = app.contacts.knock_index_for(knocker["actor_id_hex"])
        if row is not None:
            return (row,)
        app.contacts.navigate()
        return None

    (row,) = wait_until(
        _knock_row,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            f"the held knock must be listed on the child's contacts page; senders="
            f"{app.contacts.knock_senders()!r} error: {app.error_text()!r}"
        ),
    )
    card = app.driver.get_text("knock-card", index=row)
    assert knock_text in card, (
        f"a held knock stays readable to the child: the card must show its text, read {card!r}"
    )

    app.contacts.dismiss_knock(row)
    wait_until(
        lambda: app.error_text() != "",
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "the child's dismiss of a held knock must be refused and the refusal shown; "
            f"error-message stayed empty, senders={app.contacts.knock_senders()!r}"
        ),
    )
    app.contacts.navigate()

    def _still_listed():
        row = app.contacts.knock_index_for(knocker["actor_id_hex"])
        if row is not None:
            return (row,)
        app.contacts.navigate()
        return None

    wait_until(
        _still_listed,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "a refused dismiss must leave the held knock listed — it is the guardian's "
            f"to decide; senders={app.contacts.knock_senders()!r}"
        ),
    )
    assert _contact_status(url, ward_identity, knocker["actor_id_hex"]) not in (
        "accepted",
        "confirmed",
        "blocked",
    ), "the refused dismiss must leave the knock undecided on the nest"

    # ── The guardian pre-approves a contact nobody asked for, and the held
    # knocker too — the second dismisses the matching knock. ──
    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.add_contact(friend["actor_id_hex"])
    assert not app.has_error(), f"pre-approving the friend raised: {app.error_text()!r}"

    def _friend_accepted():
        status = _contact_status(url, ward_identity, friend["actor_id_hex"])
        return status if status in ("accepted", "confirmed") else None

    wait_until(
        _friend_accepted,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "a guardian's pre-approval must make the friend the child's contact with "
            "no request ever sent; status="
            f"{_contact_status(url, ward_identity, friend['actor_id_hex'])!r} "
            f"error: {app.error_text()!r}"
        ),
    )

    app.family.add_contact(knocker["actor_id_hex"])
    assert not app.has_error(), f"pre-approving the knocker raised: {app.error_text()!r}"

    def _knocker_accepted():
        status = _contact_status(url, ward_identity, knocker["actor_id_hex"])
        return status if status in ("accepted", "confirmed") else None

    wait_until(
        _knocker_accepted,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "pre-approving the held knock's sender must accept the edge; status="
            f"{_contact_status(url, ward_identity, knocker['actor_id_hex'])!r} "
            f"error: {app.error_text()!r}"
        ),
    )

    # The child's page no longer lists the knock: the pre-approval dismissed it.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)

    def _knock_gone() -> bool:
        app.contacts.navigate()
        return app.contacts.knock_index_for(knocker["actor_id_hex"]) is None

    wait_until(
        _knock_gone,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "the guardian's pre-approval must dismiss the matching knock; senders="
            f"{app.contacts.knock_senders()!r} error: {app.error_text()!r}"
        ),
    )
