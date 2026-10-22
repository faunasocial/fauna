"""tier_3 e2e: the author HOMED ON ANOTHER NEST sees the issuing nest's
takedown of their content on their own queue, named, and appeals it from there.

``moderation.md`` § Legal takedown → *The author homed on another nest* (ruled
2026-10-18): an obligation row is keyed by the author on the nest whose admin
decided, but the author's app reads its queue from its own home — so until the
nest leg, an author homed elsewhere read an empty queue and their appeal stopped
at the issuer's border. Now the issuer DELIVERS the row to the author's home
(``fauna.federation.moderation.obligation.deliver``), the home serves it on
``fauna.moderation.actions`` naming the issuer (``issuer_nest``), and the home
RELAYS the appeal back (``fauna.federation.moderation.appeal``), where the
issuer records it on its own chain. The queue row paints the issuer in its
existing text through the shared fold (``fauna_client_moderation::QueueRow``'s
``issued_by`` suffix) and publishes it as the badge's ``issuer`` fact — no new
id.

Two arms, one per kind the ruling covers, both through the UI (convention 8)
but for the peer-side setup and the read-backs:

* **A projected post** — the author on ``away`` posts; a follower on ``home``
  seeds ``away`` as a discovery contributor, so ``home`` holds a projection of
  the post arrived from ``away``; ``home``'s admin takes it down from the
  console by id.
* **A conversation message** — a community room homed on ``home`` with the
  author seated from ``away``; the founder reports the author's message and,
  signed in as ``home``'s admin, takes it down from the report row.

Then, the same for both: the author's queue on ``away`` gains the ``TakenDown``
row naming ``home`` → the appeal (reasonless refused, reason typed, submitted,
*recorded for review*) → the appeal is on ``home``'s audit chain under the
author, and ``away`` records the relay → ``home``'s admin restores → the row
survives on ``away``, still appealable.

The nest halves (forged and over-budget deliveries refused, a wrong-origin
appeal refused, a local author's row staying home) are
``bins/fauna-nest/tests/conformance_moderation_obligation_federation.rs``; the
fold is ``fauna_client_moderation::detections``' issuer tests and the paint
``apps/fauna-tui/src/moderation.rs::a_delivered_row_names_its_issuing_nest``.

**Exclusion class (8):** each nest dials the other — the delivery, the relayed
appeal and the discovery poll — so the test reads ``peer_url``
(``testing.md`` § Default app and nest mode, ruling (2)).

Latency-independent (convention 14): every wait is a named budget over a
deadline poll.

App arm: **tui** (the lead app). The six others serve the delivered row as any
local one; the issuer suffix and fact reach them with their trickle-down.

Witnesses outcome 13 of ``docs/features/moderation-queue.md`` (a coverage
contract item follows the spec and lands with its witness, outside the
public-texts boundary).
"""

from __future__ import annotations

import re
import secrets
import sqlite3
import time
import uuid

import pytest

from helpers import budgets
from helpers.admin_wire import admin_client
from helpers.mail_dedicated_nest import login_as_nest_admin
from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MESSAGE_BUDGET_S,
    Seat,
    found_community_room,
    launch_seat,
    room_app,
    seat_app_path,
    seat_story,
    teardown_seats,
    thread_by_channel,
    wait_for,
    wait_snippet,
)
from tests.api import conv_api, ws_api
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui LEADS; the six others join with their trickle-down, each with its
    # own marker (`helpers.room_seats.ROOM_APPS` already lists every app).
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
]

# The console's + the queue's own wording (i18n, en.yaml).
WORKING = "Submitting…"
DONE_TAKEDOWN = S.admin.nest_page.takedown_done
DONE_RESTORE = "Restored. The content is served again; the takedown stays on record."
APPEAL_RECORDED = "Appeal recorded. An administrator will review it."
TAKEDOWN_ACTION = "Removed under legal obligation"
REFERENCE = "Court order 299/2026"
APPEAL_REASON = "I wrote this and it breaks no law of the nest that removed it."

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # a page render after a nav
_VERDICT_BUDGET_S = 60.0  # one WS-RPC round-trip + the status repaint
# The decision crossing to the author's home: the delivery's inline first try
# plus the author's queue re-read.
_DELIVERY_BUDGET_S = budgets.CROSS_NEST_S + 30.0


def _poll(check, budget_s, tag, diagnose=None):
    """Deadline-poll ``check`` until truthy; the failure names the budget and,
    when given, what the surface showed instead (convention 6)."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    seen = f" — saw: {diagnose()!r}" if diagnose else ""
    raise AssertionError(f"{tag}: not reached within {budget_s}s{seen}")


@pytest.fixture()
def cross_nest_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(issuer, author)``: the issuer seat a fresh account on ``home``
    (``cross_nest_foreign`` — the nest that decides), later signed in as its
    admin; the author the handled actor ``away`` provisioned
    (``cross_nest_foreign_ephemeral`` — their own home). The author accepts the
    issuer seat as a contact on *their* nest, so a room invitation from it is
    admitted where the author lives."""
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        issuer = launch_seat(home, app_path, room_app, request)
        seats.append(issuer)
        author = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(author)
        conv_api.accept_contact(away["port"], author.actor, issuer.actor_hex, scheme="https")
        yield issuer, author
    finally:
        teardown_seats(seats)


def _queue_facts(app):
    m = app.moderation
    return [
        (m.row_action(i), m.row_issuer(i)) for i in range(m.correction_count())
    ]


def _queue_baseline(author_app):
    m = author_app.moderation
    m.navigate()
    assert not author_app.has_error(), author_app.error_text()
    return m.correction_count(), m.appeal_count()


def _take_down(admin, content_id, *, conversation):
    """The console's takedown of ``content_id``, driven to its verdict."""
    admin.admin.navigate_nest()
    _poll(
        lambda: admin.driver.count("admin-nest-takedown-content-id-input") > 0,
        _PAGE_BUDGET_S,
        "admin-nest renders the takedown console",
    )
    admin.admin.takedown_fill(content_id, REFERENCE, conversation=conversation)
    _arm_and_confirm(admin, DONE_TAKEDOWN, "the takedown")


def _restore(admin, content_id, *, conversation):
    """The overturn: restore mode on the same console."""
    admin.admin.navigate_nest()
    _poll(
        lambda: admin.driver.count("admin-nest-takedown-content-id-input") > 0,
        _PAGE_BUDGET_S,
        "admin-nest renders the takedown console (restore leg)",
    )
    admin.admin.takedown_fill(content_id, conversation=conversation, restore=True)
    _arm_and_confirm(admin, DONE_RESTORE, "the restore")


def _arm_and_confirm(admin, done, what):
    _poll(
        lambda: admin.driver.is_enabled("admin-nest-takedown-button"),
        _PAGE_BUDGET_S,
        f"{what} becomes armable",
    )
    admin.admin.takedown_arm()
    admin.admin.takedown_confirm()
    _poll(
        lambda: admin.driver.count("admin-nest-takedown-status") > 0
        and admin.admin.takedown_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        f"{what} reports its verdict",
    )
    verdict = admin.admin.takedown_status_text()
    assert verdict == done, (
        f"{what} must succeed, got {verdict!r} "
        f"(error-message: {admin.error_text() if admin.has_error() else 'none'})"
    )


def _delivered_row(author_app, baseline, issuer_domain):
    """The author's queue on their own nest gains exactly one ``TakenDown``
    row naming the issuing nest, with one more appeal handle; returns its
    index."""
    rows, appeals = baseline
    m = author_app.moderation
    m.navigate()
    wanted = (TAKEDOWN_ACTION, issuer_domain)
    _poll(
        lambda: wanted in _queue_facts(author_app),
        _DELIVERY_BUDGET_S,
        f"the author's queue on their own nest gains the TakenDown row naming {issuer_domain!r}",
        diagnose=lambda: (
            _queue_facts(author_app),
            author_app.error_text() if author_app.has_error() else "",
        ),
    )
    assert m.correction_count() == rows + 1, (
        f"one decision must add exactly one queue row: {_queue_facts(author_app)!r}"
    )
    assert m.appeal_count() == appeals + 1, (
        "the delivered row must offer exactly one appeal handle, as a local one does"
    )
    return _queue_facts(author_app).index(wanted)


def _appeal(author_app, row_index, rows_expected):
    """The appeal flow, exactly as the single-nest journeys drive it."""
    m = author_app.moderation
    m.open_appeal(row_index)
    _poll(m.appeal_form_open, _PAGE_BUDGET_S, "the appeal form opens on the row")
    assert not m.appeal_submit_enabled(), "a reasonless appeal must not be submittable"
    m.fill_appeal_reason(APPEAL_REASON)
    _poll(m.appeal_submit_enabled, _PAGE_BUDGET_S, "a reasoned appeal becomes submittable")
    m.submit_appeal()
    _poll(
        lambda: m.has_appeal_status() and m.appeal_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        "the appeal reports its outcome",
    )
    outcome = m.appeal_status_text()
    assert outcome == APPEAL_RECORDED, (
        f"the issuer must record the relayed appeal, got {outcome!r} "
        f"(error-message: {author_app.error_text() if author_app.has_error() else 'none'})"
    )
    assert not author_app.has_error(), "a recorded appeal is not a page error"
    assert m.correction_count() == rows_expected, (
        "an appeal is additive history — it must not remove the queue row"
    )


def _newest_audit(nest, action):
    """The newest audit entry carrying ``action`` on ``nest``'s chain — a read
    (convention 8 allows it), never a mutation."""
    with admin_client(nest) as client:
        entries = client.call("fauna.admin.audit.list", {"limit": 50})["entries"]
    return next((e for e in entries if e["action"] == action), None), entries


def _appeal_on_chains(home, away, author_actor):
    """The appeal is the ISSUER's record — on ``home``'s chain under the
    author — and ``away`` records only that it relayed it."""
    author_id = bytes(author_actor["actor_id_bytes"])
    appeal, entries = _newest_audit(home, "moderation:appeal")
    assert appeal is not None and bytes(appeal["actor_id"]) == author_id, (
        "the relayed appeal must be on the issuer's audit chain under the author: "
        f"{[(e['action'], e.get('target')) for e in entries[:8]]!r}"
    )
    relayed, entries = _newest_audit(away, "moderation:appeal-relayed")
    assert relayed is not None and bytes(relayed["actor_id"]) == author_id, (
        "the author's home must record the relay under the author: "
        f"{[(e['action'], e.get('target')) for e in entries[:8]]!r}"
    )


def _row_survives(author_app, rows_expected, issuer_domain):
    m = author_app.moderation
    m.navigate()
    _poll(
        lambda: m.correction_count() == rows_expected
        and (TAKEDOWN_ACTION, issuer_domain) in _queue_facts(author_app),
        _VERDICT_BUDGET_S,
        "the delivered row survives the issuer's restore (additive history)",
        diagnose=lambda: _queue_facts(author_app),
    )
    assert m.appeal_count() >= 1, "an overturned decision's row is still appealable"


def _projected_on(nest, post_id_hex, origin_url):
    """Whether ``nest`` holds a projection of the post that arrived from
    ``origin_url`` — a read of its own store."""
    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT origin_nest_url FROM content WHERE id = ?",
            (bytes.fromhex(post_id_hex),),
        ).fetchone()
    finally:
        conn.close()
    return row is not None and bool(row[0]) and origin_url.rstrip("/") in row[0]


@pytest.mark.feature("moderation-queue")
def test_the_author_on_another_nest_appeals_a_takedown_of_their_projected_post(
    cross_nest_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """the author on ``away`` posts → ``home`` projects it → ``home``'s admin
    takes it down → the author's queue on ``away`` gains the row naming
    ``home`` → the appeal is recorded on ``home`` → ``home`` restores → the row
    survives."""
    issuer, author = cross_nest_seats
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    away_peer = away["peer_url"]
    token = secrets.token_hex(5)
    text = f"crossnestappeal{token} a post another nest projects"

    baseline = _queue_baseline(author.app)
    author.app.feed.navigate()
    author.app.feed.create_post(text)
    row = author.app.feed.wait_for_post_state_by_text(text)
    assert row is not None, "the post must land in the author's feed state"
    post_id = row["post_id"]

    # Peer-side setup, not the user under test: a reader on `home` seeds
    # `away` as a discovery contributor, so `home` fetches the post and indexes
    # its projection with `away` as its origin — the copy `home`'s admin acts
    # on, and how `home` finds the author's home to deliver to.
    from common.auth import create_actor_and_register

    reader = create_actor_and_register(
        home["port"], admin_signing_key=home["admin"]["signing_key"]
    )
    ws_api.create_feed(
        home["port"],
        reader,
        f"cross-nest appeal {token}",
        rules=[{"BodyContains": {"terms": [f"crossnestappeal{token}"]}}],
        scope="discovery",
        contributor_seeds=[away_peer],
    )
    _poll(
        lambda: _projected_on(home, post_id, away_peer),
        budgets.FEDERATION_EXCHANGE_CYCLE_S,
        "the issuing nest holding a projection of the post from the author's home",
    )

    login_as_nest_admin(issuer.app, home, home["url"])
    _take_down(issuer.app, post_id, conversation=False)

    index = _delivered_row(author.app, baseline, home["authority"])
    rows = baseline[0] + 1
    _appeal(author.app, index, rows)
    _appeal_on_chains(home, away, author.actor)

    _restore(issuer.app, post_id, conversation=False)
    _row_survives(author.app, rows, home["authority"])


_RECORD_ID = re.compile(r"\b[0-9a-f]{64}\b")


@pytest.mark.feature("moderation-queue")
def test_the_author_on_another_nest_appeals_a_takedown_of_their_room_message(
    cross_nest_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """a room homed on ``home`` seats the author from ``away`` → the author
    writes → the founder reports it and, as ``home``'s admin, takes it down
    from the report row → the author's queue on ``away`` gains the row naming
    ``home`` → the appeal is recorded on ``home`` → ``home`` restores → the row
    survives."""
    issuer, author = cross_nest_seats
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    assert away["peer_url"], "the issuer dials the author's home (class 8)"
    words = f"crossnestbody{uuid.uuid4().hex[:10]}"
    note = f"cross-nest appeal {uuid.uuid4().hex[:10]}"

    room_id, issuer_thread, author_thread = found_community_room(
        issuer, author, f"hello {secrets.token_hex(4)}",
        address=f"{author.handle}@{away['authority']}",
    )
    baseline = _queue_baseline(author.app)

    # The order the cross-nest room journey proves: a message from the founder
    # opens on the author's seat across the relay before the author writes.
    greeting = f"welcome{secrets.token_hex(4)}"
    issuer.app.conversations.send_in_open_thread(f"glad you came {greeting}")
    wait_snippet(author, room_id, greeting, what="the founder's message opening on the author's seat")
    author.app.conversations.open_thread_by_id(author_thread)
    author.app.conversations.send_in_open_thread(words)
    wait_for(
        "the founder receiving the author's message",
        lambda: thread_by_channel(issuer.app, room_id),
        lambda t: t is not None and words in (t.snippet or ""),
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"{seat_story(issuer)}\n{seat_story(author)}",
    )

    issuer.app.conversations.open_thread_by_id(issuer_thread)
    d = issuer.app.driver
    texts = wait_for(
        f"a bubble carrying {words!r} painting",
        lambda: [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))],
        lambda ts: any(words in t for t in ts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(issuer),
    )
    issuer.app.conversations.open_message_actions(
        message_index=next(i for i, t in enumerate(texts) if words in t)
    )
    issuer.app.report.open_from_message_menu()
    issuer.app.report.choose_reason("other")
    issuer.app.report.fill_note(note)
    _poll(issuer.app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    issuer.app.report.submit()
    _poll(issuer.app.report.has_status, _VERDICT_BUDGET_S, "the report is acknowledged")

    # The founder's app signs in as `home`'s admin, which learns the record id
    # the way the product hands it over: the report row.
    admin = issuer.app
    login_as_nest_admin(admin, home, home["url"])
    admin.admin.navigate_nest()
    _poll(
        lambda: admin.driver.count("admin-nest-reports-section") > 0
        and admin.admin.report_index(note) >= 0,
        _PAGE_BUDGET_S,
        "the message report waits in the issuing nest's admin queue",
        diagnose=admin.admin.report_rows,
    )
    rows_seen = admin.admin.report_rows()
    at = admin.admin.report_index(note)
    admin.admin.report_open_takedown(sum(1 for r in rows_seen[:at] if " actor " not in r))
    _poll(
        lambda: len(admin.admin.takedown_content_id()) == 64,
        _PAGE_BUDGET_S,
        "open takedown pre-fills the console with the message's record id",
    )
    record_id = admin.admin.takedown_content_id()
    assert _RECORD_ID.fullmatch(record_id), record_id
    admin.admin.takedown_cite(REFERENCE)
    _arm_and_confirm(admin, DONE_TAKEDOWN, "the takedown")

    index = _delivered_row(author.app, baseline, home["authority"])
    rows = baseline[0] + 1
    _appeal(author.app, index, rows)
    _appeal_on_chains(home, away, author.actor)

    _restore(admin, record_id, conversation=True)
    _row_survives(author.app, rows, home["authority"])
