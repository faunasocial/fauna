"""The contact-edge lifecycle on the Contacts page, driven through the app's own UI.

Three outcomes of ``docs/features/contacts.md``, one journey each:

* accepting a knock adds that person to your contacts, and blocking or dismissing
  one clears it from the list;
* confirming an accepted contact promotes it, and ``contact-confirm`` is offered
  only on a row it applies to (the ``status='accepted'`` one — ``contacts.md``
  § Layout & flow region 2, ruled in person 2026-08-15);
* every roster row's ``contact-status`` says where that relationship stands,
  through the shared ``fauna_core::format::contact_status_label``.

The knocks are stored by the nest's ``POST /api/v1/test/push/knock`` (the
``test-hooks`` feature — the production ``db.push_knock`` row plus a real
``PushEvent::Knock``, as in ``test_knock_live_refresh.py``), each from a FRESH
sender: the driven actor is session-scoped, so rows from earlier tests are
always present and every assertion here is about the rows this test made. The
roster is read through the page's own local filter (``contacts-search-field``
over the shared ``contact_matches_filter``), which a peer's full actor-id hex
narrows to exactly that peer's row.

Every wait is a deadline poll on the state the gesture produces, never a settle
(convention 14). tier_2: the knock trigger is the test hook; the accept, block,
dismiss and confirm are the app's real WS-RPC calls.
"""
from __future__ import annotations

import uuid

import pytest
import requests

from actions.api_actor import ApiActor
from helpers import budgets
from helpers.connection import wait_until_online
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_2


def _fresh_actor_hex() -> str:
    return uuid.uuid4().hex + uuid.uuid4().hex  # 64 hex chars = 32 bytes


def _store_knock(nest_instance, test_user, sender_hex: str) -> None:
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/knock",
        json={
            "actor_id": test_user["actor_id_hex"],
            "sender_id": sender_hex,
            "summary": f"edge lifecycle probe {sender_hex[:8]}",
        },
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks knock endpoint returned {resp.status_code}: {resp.text}"
    )
    assert resp.json().get("ok") is True, resp.text


def _open_people(app) -> None:
    """The Contacts page on its People half. The segment is sticky across
    navigation, so an earlier Address Book visit would otherwise leave the
    roster and the knock list unpainted."""
    app.contacts.navigate()
    app.contacts.switch_to_people()


def _read(fn):
    """One poll's read of a list the page may re-render under it. A read racing
    a re-render answers ``None`` and the next poll reads the whole list again;
    one that never succeeds times out into the caller's diagnosis."""
    try:
        return fn()
    except Exception:  # noqa: BLE001 — see the docstring
        return None


def _knock_row(app, sender_hex: str) -> int:
    """Wait for ``sender_hex``'s knock to list, and return its row index."""
    found = wait_until(
        lambda: (lambda i: (i,) if i is not None else None)(
            _read(lambda: app.contacts.knock_index_for(sender_hex))
        ),
        budgets.PUSH_REFRESH_S,
        diagnose=lambda: (
            f"the knock from {sender_hex} never listed; knock-sender texts="
            f"{_read(app.contacts.knock_senders)!r}; error={app.error_text()!r}"
        ),
    )
    return found[0]


def _wait_knock_gone(app, sender_hex: str, verb: str) -> None:
    wait_until(
        lambda: _read(lambda: app.contacts.knock_index_for(sender_hex) is None),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{verb} left the knock from {sender_hex} on the list; knock-sender "
            f"texts={_read(app.contacts.knock_senders)!r}; error={app.error_text()!r}"
        ),
    )


def _wait_row_statuses(app, peer_hex: str, allowed: list[list[str]], why: str) -> None:
    """Filter the roster to ``peer_hex`` and wait until the rows it keeps read
    one of ``allowed`` — ``[label]`` when the peer is a contact at that status,
    ``[]`` when there is no edge at all."""
    app.contacts.narrow_roster(peer_hex)
    wait_until(
        lambda: _read(app.contacts.contact_statuses) in allowed,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{why}: the roster filtered to {peer_hex} should read one of "
            f"{allowed!r}; got {_read(app.contacts.contact_statuses)!r} "
            f"(contact-name count {app.contacts.contact_count()}); "
            f"error={app.error_text()!r}"
        ),
    )


def _wait_row_status(app, peer_hex: str, expected: str, why: str) -> None:
    _wait_row_statuses(app, peer_hex, [[expected]], why)


@pytest.mark.feature("contacts")
def test_accepting_a_knock_adds_the_person_and_blocking_or_dismissing_clears_it(
    logged_in_app, nest_instance, test_user,
):
    """Three knocks; accept one, block one, dismiss one — each through its own
    button on its own row. The accepted sender becomes an ``accepted`` contact;
    all three leave the knock list; the dismissed one leaves no edge at all."""
    app = logged_in_app
    wait_until_online(app.driver)

    accepted, blocked, dismissed = (_fresh_actor_hex() for _ in range(3))
    for sender in (accepted, blocked, dismissed):
        _store_knock(nest_instance, test_user, sender)
    _open_people(app)
    for sender in (accepted, blocked, dismissed):
        _knock_row(app, sender)

    # Row indices are re-read before every click: each verb re-reads the list,
    # so the rows below it move up.
    app.contacts.accept_knock(_knock_row(app, accepted))
    _wait_knock_gone(app, accepted, "accepting")
    _wait_row_status(
        app, accepted, S.common.accepted, "accepting a knock must add that person"
    )
    app.contacts.narrow_roster("")

    app.contacts.block_knock(_knock_row(app, blocked))
    _wait_knock_gone(app, blocked, "blocking")
    # A block writes a `blocked` edge (so the sender cannot knock again), which
    # the roster may list — but never as a contact the viewer accepted.
    _wait_row_statuses(
        app, blocked, [[], [S.common.blocked]], "a blocked sender must not become a contact"
    )
    app.contacts.narrow_roster("")

    app.contacts.dismiss_knock(_knock_row(app, dismissed))
    _wait_knock_gone(app, dismissed, "dismissing")
    # Dismiss clears the knock and any edge with it, so the sender may knock again.
    _wait_row_statuses(
        app, dismissed, [[]], "dismissing a knock must leave no contact row for its sender"
    )
    app.contacts.narrow_roster("")

    assert not app.has_error(), (
        f"the knock verbs must not surface an error; got {app.error_text()!r}"
    )


@pytest.mark.feature("contacts")
def test_confirming_an_accepted_contact_promotes_it_and_confirm_shows_only_where_it_applies(
    logged_in_app, nest_instance, test_user,
):
    """Accept a knock, confirm the resulting contact from its row: the row flips
    from Accepted to Confirmed and its confirm affordance goes with it. Across
    the whole roster, ``contact-confirm`` is offered on exactly the Accepted
    rows (convention 17 — the general invariant, not one hand-picked row)."""
    app = logged_in_app
    wait_until_online(app.driver)

    peer = _fresh_actor_hex()
    _store_knock(nest_instance, test_user, peer)
    _open_people(app)
    app.contacts.accept_knock(_knock_row(app, peer))
    _wait_row_status(app, peer, S.common.accepted, "accepting the knock")
    assert app.contacts.confirm_count() == 1, (
        f"an accepted row must offer contact-confirm; "
        f"{app.driver.diagnose('contact-confirm')}"
    )

    app.contacts.confirm_contact(0)
    _wait_row_status(app, peer, S.common.confirmed, "confirming an accepted contact")
    wait_until(
        lambda: app.contacts.confirm_count() == 0,
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            f"a confirmed row must carry no confirm affordance; "
            f"{app.driver.diagnose('contact-confirm')}"
        ),
    )

    # The whole roster: one confirm per Accepted row, none anywhere else.
    app.contacts.narrow_roster("")
    wait_until(
        lambda: _read(app.contacts.contact_statuses),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the unfiltered roster never repainted; error={app.error_text()!r}",
    )
    statuses = app.contacts.contact_statuses()
    offered = app.contacts.confirm_count()
    assert offered == statuses.count(S.common.accepted), (
        f"contact-confirm must render on the Accepted rows only: {offered} "
        f"offered against statuses {statuses!r}"
    )
    assert not app.has_error(), (
        f"the confirm must not surface an error; got {app.error_text()!r}"
    )


@pytest.mark.feature("contacts")
def test_each_contact_row_says_where_the_relationship_stands(
    logged_in_app, nest_instance, test_user,
):
    """An accepted, a confirmed and a blocked contact each read their own status
    label on their own row; and every row on the page reads one of the shared
    labels, one per row (convention 17)."""
    app = logged_in_app
    viewer = ApiActor(
        nest_instance["url"], test_user["token"], test_user["actor_id_hex"],
        bytes(test_user["signing_key"]),
    )
    # The edges are the precondition, set over the wire; what is under test is
    # what the page says about them.
    accepted, confirmed, blocked = (_fresh_actor_hex() for _ in range(3))
    viewer.accept_knock(accepted)
    viewer.accept_knock(confirmed)
    viewer.confirm_contact(confirmed)
    viewer.block_knock(blocked)

    _open_people(app)
    for peer, label in (
        (accepted, S.common.accepted),
        (confirmed, S.common.confirmed),
        (blocked, S.common.blocked),
    ):
        _wait_row_status(app, peer, label, f"a {label.lower()} contact's row")

    app.contacts.narrow_roster("")
    wait_until(
        lambda: _read(app.contacts.contact_statuses),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the unfiltered roster never repainted; error={app.error_text()!r}",
    )
    statuses = app.contacts.contact_statuses()
    labels = {S.common.pending, S.common.accepted, S.common.confirmed, S.common.blocked}
    assert len(statuses) == app.contacts.contact_count(), (
        f"every roster row must carry its status: {len(statuses)} contact-status "
        f"against {app.contacts.contact_count()} contact-name"
    )
    off_vocabulary = [s for s in statuses if s not in labels]
    assert not off_vocabulary, (
        f"every row's status must be one of the shared labels {sorted(labels)!r}; "
        f"these are not: {off_vocabulary!r}"
    )
