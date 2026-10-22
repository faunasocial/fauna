"""tier_3 e2e: the author of a CONVERSATION message appeals its withholding
from their moderation queue — a legal takedown and an admin block alike.

``moderation.md`` § Legal takedown → *Conversations* (ruled 2026-10-18): the
relay holds no plaintext, but it holds the record's nest-attested author
(``conv_record_authors``), so a takedown of an attested record writes the SAME
``TakenDown`` row on the author's queue a post takedown writes — the
transparency triple's appeal leg is the author's queue row, with zero new app
UI. § Admin block of a reported item → *Mechanism*: a block of a conversation
record keys its ``Blocked`` row on the attested author too, ``category`` the
report's reason. The post twin is ``test_moderation_appeal.py``; the nest half
(the appeal scoped to the attested author, a stranger refused) is
``tests/api/test_moderation_appeal.py::test_a_conversation_appeal_is_the_attested_authors``
(convention 5).

Both journeys, all through the UI (e2e-conventions point 8 — no API call
stands in for the user):

a room of three → the author writes it → a member reports the message from
its menu → that member's app signs in as the nest admin, which learns the
record id the way the product hands it over (the report row) → **takedown**
(open takedown → cite → arm → confirm) or **Block** (from the report row) →
the author's own queue gains the row with its appeal handle → the appeal:
reasonless refused, reason typed, submitted, *recorded for review*, the row
stays → the admin **restores** / **Unblocks** → the row survives as history,
still appealable.

The author's own thread says it too (§ Legal takedown → *App render*, the
fourth surface): their copy keeps its body, and ``dm-message-withheld-badge``
beside it — matched to the queue row by record digest when the thread opens —
names the ground and opens that row's appeal. The takedown journey appeals
FROM the badge (it must land on the queue's own appeal form, one flow); the
block journey reads the badge and appeals from the queue row.

The author's queue is read on the author's own seat, which never signs in as
anyone else: the queue is the author's one signal, since their own devices
keep the real copy and never receive the tombstone (§ Legal takedown →
*Conversations*).

Latency-independent (convention 14): every wait is a named generous budget
over a deadline poll.

App arms: **tui** (the lead app), then **web**, **linux** and **android** (the
badge's trickle-down); windows, macos and ios join
with their legs, each adding its marker here. Every app paints the same queue
and appeal, which branch on no ``content_type`` (§ Implementation status
today); their arms of the report and block surfaces are also witnessed by
``test_admin_block.py`` and ``test_legal_takedown_notice.py``.
"""

import re
import time
import uuid

import pytest

from helpers.mail_dedicated_nest import login_as_nest_admin
from i18n.strings import S
from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MESSAGE_BUDGET_S,
    bootstrap_room,
    room_app,
    room_seats,
    seat_story,
    wait_for,
    wait_snippet,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.linux, pytest.mark.android]

# The console's + the queue's own wording (i18n, en.yaml) — the shared verdicts
# every app renders (`fauna_client_moderation::{takedown,appeal,admin_block}`).
WORKING = "Submitting…"
DONE_TAKEDOWN = S.admin.nest_page.takedown_done
DONE_RESTORE = "Restored. The content is served again; the takedown stays on record."
APPEAL_RECORDED = "Appeal recorded. An administrator will review it."
# A takedown row: the `illegal` enforcement descriptor + the TakenDown label —
# the post twin's exact facts, since the nest writes one row shape for both.
TAKEDOWN_CATEGORY = "Illegal"
TAKEDOWN_ACTION = "Removed under legal obligation"
# A block row: the report's reason as its category + the Blocked label.
BLOCK_REASON = "harassment"
BLOCK_CATEGORY = "Harassment"
BLOCKED_ACTION = "Blocked by this nest's admin"

APPEAL_REASON = "I wrote this message and it breaks no law or rule."

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # a page render after a nav
_VERDICT_BUDGET_S = 60.0  # one WS-RPC round-trip + the status repaint
_QUEUE_BUDGET_S = 60.0    # the author's queue re-read reflecting the decision

_RECORD_ID = re.compile(r"\b[0-9a-f]{64}\b")


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


def _bubble_index(seat, needle):
    """The index of the live bubble carrying ``needle`` — also its
    `dm-message-actions-button` index."""
    d = seat.app.driver
    texts = wait_for(
        f"a bubble carrying {needle!r} painting",
        lambda: [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))],
        lambda ts: any(needle in t for t in ts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )
    return next(i for i, t in enumerate(texts) if needle in t)


def _queue_facts(app):
    m = app.moderation
    return [
        (m.row_category(i), m.row_action(i)) for i in range(m.correction_count())
    ]


def _send_and_report(seats, reason, note):
    """The room, the author's message, and a member's report of it from the
    message menu. Returns the author's queue baseline (rows, appeal handles) —
    taken before any decision, so every row asserted later is this one's —
    the author's thread id and the message's words."""
    author_seat, reporter, _ = seats
    owner_thread, channel = bootstrap_room(*seats)
    token = uuid.uuid4().hex[:10]
    words = f"appealbody{token}"

    author_seat.app.moderation.navigate()
    assert not author_seat.app.has_error()
    baseline = (
        author_seat.app.moderation.correction_count(),
        author_seat.app.moderation.appeal_count(),
    )

    author_seat.app.conversations.real_send(owner_thread, words)
    received = wait_snippet(reporter, channel, words, what="the member receiving the message")
    reporter.app.conversations.open_thread_by_id(received.thread_id)
    reporter.app.conversations.open_message_actions(message_index=_bubble_index(reporter, words))
    reporter.app.report.open_from_message_menu()
    reporter.app.report.choose_reason(reason)
    reporter.app.report.fill_note(note)
    _poll(reporter.app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    reporter.app.report.submit()
    _poll(reporter.app.report.has_status, _VERDICT_BUDGET_S, "the report is acknowledged")
    return baseline, owner_thread, words


def _author_badge(author_app, owner_thread, words, action):
    """The author opens their own thread: the message keeps its body and the
    withheld badge beside it names ``action`` — read when the thread opens, so
    a re-open after the decision is what paints it."""
    c = author_app.conversations
    c.navigate()
    c.open_thread_by_id(owner_thread)
    _poll(
        lambda: any(action in b for b in c.withheld_badges()),
        _QUEUE_BUDGET_S,
        f"the author's own copy gains the withheld badge naming {action!r}",
        diagnose=lambda: (c.withheld_badges(), author_app.error_text() if author_app.has_error() else ""),
    )
    badges = c.withheld_badges()
    assert len(badges) == 1, f"one badge, on the one withheld message: {badges!r}"
    assert badges[0].endswith("Appeal"), f"the badge offers the appeal: {badges[0]!r}"
    texts = [author_app.driver.get_text("dm-message-text", index=i) for i in range(author_app.driver.count("dm-message-text"))]
    assert any(words in (t or "") for t in texts), (
        "the author's copy keeps its body beside the badge — never collapsed to a tombstone"
    )


def _admin_finds_report(admin, nest_instance, note):
    """Sign the reporter's app in as the nest admin and wait for the report
    row; returns its item-button index (account rows paint no item buttons)."""
    login_as_nest_admin(admin, nest_instance, nest_instance["url"])
    admin.admin.navigate_nest()
    _poll(
        lambda: admin.driver.count("admin-nest-reports-section") > 0
        and admin.admin.report_index(note) >= 0,
        _PAGE_BUDGET_S,
        "the message report waits in the admin queue",
        diagnose=admin.admin.report_rows,
    )
    rows = admin.admin.report_rows()
    row = admin.admin.report_index(note)
    return sum(1 for r in rows[:row] if " actor " not in r)


def _open_queue(author_app):
    """Land on the author's moderation queue FRESH: a page the app is already
    on is not re-read by a second navigation on every app (linux and web read
    the queue on arrival), so step off to the conversations page first."""
    author_app.conversations.navigate()
    author_app.moderation.navigate()


def _wait_author_row(author_app, baseline, action):
    """The author's queue gains exactly one row carrying ``action`` and one
    more appeal handle; returns that row's index."""
    rows, appeals = baseline
    m = author_app.moderation
    _open_queue(author_app)
    _poll(
        lambda: any(a == action for _, a in _queue_facts(author_app)),
        _QUEUE_BUDGET_S,
        f"the author's moderation queue gains the {action!r} row",
        diagnose=lambda: (_queue_facts(author_app), author_app.error_text() if author_app.has_error() else ""),
    )
    assert m.correction_count() == rows + 1, (
        f"one decision must add exactly one queue row: {_queue_facts(author_app)!r}"
    )
    assert m.appeal_count() == appeals + 1, (
        "the nest-issued row on a conversation message must offer exactly one "
        "appeal handle, as a post's does"
    )
    return next(i for i, (_, a) in enumerate(_queue_facts(author_app)) if a == action)


def _appeal(author_app, row_index, rows_expected, from_badge=False):
    """The appeal flow, exactly as the post twin drives it: guard → reason →
    recorded → the row stays, still appealable. Every row on the author's
    fresh account is nest-issued, so the row's index is its appeal index.
    ``from_badge`` opens it from the author's thread instead — the badge must
    land on this same form."""
    m = author_app.moderation
    if from_badge:
        author_app.conversations.open_withheld_appeal(0)
    else:
        m.open_appeal(row_index)
    _poll(
        m.appeal_form_open,
        _PAGE_BUDGET_S,
        "the appeal form opens on the row" + (" from the thread's badge" if from_badge else ""),
        diagnose=lambda: author_app.error_text() if author_app.has_error() else "",
    )
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
        f"the author's appeal of a conversation decision must be recorded, got "
        f"{outcome!r} (error-message: {author_app.error_text() if author_app.has_error() else 'none'})"
    )
    assert not author_app.has_error(), "a recorded appeal is not a page error"
    assert not m.appeal_form_open(), "a recorded appeal closes its form"
    assert m.correction_count() == rows_expected, (
        "an appeal is additive history — it must not remove the queue row"
    )
    assert m.appeal_count() >= 1, "the decision is still on record, so its handle is still offered"


def _row_survives(author_app, rows_expected, action):
    """After the overturn, the row survives as history and stays appealable."""
    m = author_app.moderation
    _open_queue(author_app)
    _poll(
        lambda: m.correction_count() == rows_expected
        and any(a == action for _, a in _queue_facts(author_app)),
        _QUEUE_BUDGET_S,
        "the decision's row survives the overturn (additive history)",
        diagnose=lambda: _queue_facts(author_app),
    )
    assert m.appeal_count() >= 1, "an overturned decision's row is still appealable"


@pytest.mark.real_conversations
@pytest.mark.feature("moderation-queue")
def test_the_author_appeals_a_conversation_takedown_from_the_queue(room_seats, nest_instance):
    """a member reports the author's message → the admin takes it down from
    the report row → the author's queue gains the TakenDown row (Illegal,
    "Removed under legal obligation") with its appeal handle → the appeal is
    recorded → the admin restores → the row survives, still appealable."""
    author, reporter, third = room_seats
    note = f"conversation appeal takedown {uuid.uuid4().hex[:10]}"
    baseline, owner_thread, words = _send_and_report((author, reporter, third), "other", note)

    admin = reporter.app
    button = _admin_finds_report(admin, nest_instance, note)
    admin.admin.report_open_takedown(button)
    _poll(
        lambda: len(admin.admin.takedown_content_id()) == 64,
        _PAGE_BUDGET_S,
        "open takedown pre-fills the console with the message's record id",
    )
    record_id = admin.admin.takedown_content_id()
    admin.admin.takedown_cite("Court order 297/2026")
    _poll(
        lambda: admin.driver.is_enabled("admin-nest-takedown-button"),
        _PAGE_BUDGET_S,
        "a cited takedown becomes armable",
    )
    admin.admin.takedown_arm()
    assert f"conversation {record_id}" in admin.admin.takedown_confirm_summary()
    admin.admin.takedown_confirm()
    _poll(
        lambda: admin.driver.count("admin-nest-takedown-status") > 0
        and admin.admin.takedown_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        "the takedown reports its verdict",
    )
    assert admin.admin.takedown_status_text() == DONE_TAKEDOWN, (
        f"the takedown must succeed, got {admin.admin.takedown_status_text()!r}"
    )

    # --- The author's queue: the triple's appeal leg, on a message.
    row = _wait_author_row(author.app, baseline, TAKEDOWN_ACTION)
    category = author.app.moderation.row_category(row)
    assert TAKEDOWN_CATEGORY in category, (
        f"the row names what the message was withheld as: {_queue_facts(author.app)!r}"
    )
    rows = baseline[0] + 1
    # --- The author's own thread: the badge, and the appeal from it.
    _author_badge(author.app, owner_thread, words, TAKEDOWN_ACTION)
    _appeal(author.app, row, rows, from_badge=True)

    # --- The overturn: restore mode on the same console, conversation kind.
    admin.admin.navigate_nest()
    _poll(
        lambda: admin.driver.count("admin-nest-takedown-content-id-input") > 0,
        _PAGE_BUDGET_S,
        "admin-nest renders the takedown console (restore leg)",
    )
    admin.admin.takedown_fill(record_id, conversation=True, restore=True)
    _poll(
        lambda: admin.driver.is_enabled("admin-nest-takedown-button"),
        _PAGE_BUDGET_S,
        "a restore with no note is armable",
    )
    admin.admin.takedown_arm()
    admin.admin.takedown_confirm()
    _poll(
        lambda: admin.driver.count("admin-nest-takedown-status") > 0
        and admin.admin.takedown_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        "the restore reports its verdict",
    )
    assert admin.admin.takedown_status_text() == DONE_RESTORE, (
        f"the restore must succeed, got {admin.admin.takedown_status_text()!r}"
    )
    _row_survives(author.app, rows, TAKEDOWN_ACTION)


@pytest.mark.real_conversations
@pytest.mark.feature("moderation-queue")
def test_the_author_appeals_a_conversation_block_from_the_queue(room_seats, nest_instance):
    """a member reports the author's message for harassment → the admin Blocks
    it from the report row → the author's queue gains the Blocked row whose
    category is the report's reason, with its appeal handle → the appeal is
    recorded → the admin Unblocks → the row survives, still appealable."""
    author, reporter, third = room_seats
    note = f"conversation appeal block {uuid.uuid4().hex[:10]}"
    baseline, owner_thread, words = _send_and_report((author, reporter, third), BLOCK_REASON, note)

    admin = reporter.app
    button = _admin_finds_report(admin, nest_instance, note)
    admin.admin.report_block(button)
    _poll(
        lambda: admin.driver.count("admin-nest-report-block-confirm-button") > 0,
        _PAGE_BUDGET_S,
        "arming paints the block confirm",
    )
    summary = admin.admin.block_confirm_summary()
    assert summary.startswith("Block") and BLOCK_CATEGORY in summary, summary
    found = _RECORD_ID.search(summary)
    assert found, f"the confirm names the message's record id: {summary!r}"
    record_id = found.group(0)
    admin.admin.block_confirm()
    _poll(
        lambda: admin.admin.blocked_index(record_id) >= 0,
        _VERDICT_BUDGET_S,
        "the blocked message joins the blocked list",
        diagnose=lambda: (admin.admin.blocked_rows(), admin.error_text() if admin.has_error() else ""),
    )

    # --- The author's queue: the block's appeal leg, on a message.
    row = _wait_author_row(author.app, baseline, BLOCKED_ACTION)
    category = author.app.moderation.row_category(row)
    assert BLOCK_CATEGORY in category, (
        f"a block row's category is the report's reason: {_queue_facts(author.app)!r}"
    )
    rows = baseline[0] + 1
    # The author's copy was synced before the block, so the badge is its one
    # notice; the appeal stays the queue row's.
    _author_badge(author.app, owner_thread, words, BLOCKED_ACTION)
    _open_queue(author.app)
    _appeal(author.app, row, rows)

    # --- The overturn: Unblock, from the blocked list.
    admin.admin.navigate_nest()
    _poll(
        lambda: admin.admin.blocked_index(record_id) >= 0,
        _PAGE_BUDGET_S,
        "the blocked message is listed",
        diagnose=admin.admin.blocked_rows,
    )
    admin.admin.unblock(admin.admin.blocked_index(record_id))
    _poll(
        lambda: admin.driver.count("admin-nest-report-block-confirm-button") > 0,
        _PAGE_BUDGET_S,
        "arming the unblock paints the confirm",
    )
    assert admin.admin.block_confirm_summary().startswith("Unblock")
    admin.admin.block_confirm()
    _poll(
        lambda: admin.admin.blocked_index(record_id) < 0,
        _VERDICT_BUDGET_S,
        "an unblocked message leaves the blocked list",
        diagnose=admin.admin.blocked_rows,
    )
    _row_survives(author.app, rows, BLOCKED_ACTION)
