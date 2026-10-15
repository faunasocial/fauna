"""tier_3 e2e: content taken down under a legal obligation shows a notice IN ITS
PLACE — in a quote of it and in the opened post — never a blank or an error.

``moderation.md`` § Legal takedown (the app render: "three surfaces, one
concept … each paints the tombstone IN PLACE OF the withheld content, never a
blank surface and never a page-level error standing in for a body"), witnessed
for ``feed-read`` outcome 11's two post surfaces.

The journey is the one that bites: the reader had ALREADY seen the post. The
admin posts it, quotes it and reposts it, so this device resolved both embeds
from its own loaded page with the words in them; then the takedown lands
through the admin console, and a compose — the feed's re-pull barrier — is all
that happens before the reads. The quote and the repost row must now carry the
shared tombstone instead of the words they showed a moment ago (the feed
manager forgets resolved quotes at every reload, `ui/feed.md` § The read
model), and opening the repost row — which opens its ORIGINAL — must paint the
tombstone in the detail's body, with no page error.

Mutations are UI-only (e2e-conventions point 8): post, quote, repost,
takedown and the barrier compose all go through the app; the only
state-protocol reads are the post ids the console and the id-keyed card reads
need.

App arm: **tui** (the lead app).

The conversation surface of the same outcome is the second journey, and its
shape follows two facts of the design. The nest withholds a taken-down message
only from FUTURE fetches (``moderation.md`` § Legal takedown → Conversations:
a device that already synced it keeps its copy), so the reader is a room
member whose app is closed while the message is sent and taken down, and whose
first fetch of it follows the takedown. And the admin learns the message's
record id the way the product hands it over: a member reports the message, and
the queue row's *open takedown* pre-fills the console's conversation half
(``moderation.md`` § User-initiated reporting → Where it lands). The same
journey witnesses ``admin-nest``'s single-message takedown. It runs on tui
alone: every app paints the conversation tombstone as presentation with no
test id, and only tui's driver can read it, off the terminal stream the driver
itself is.
"""

import time
import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.connection import wait_until_online
from helpers.conversations_restart import wait_replica_settled
from helpers.mail_dedicated_nest import login_as_nest_admin
from helpers.room_seats import (  # noqa: F401 — room_app/room_seats are pytest fixtures adopted by import
    MESSAGE_BUDGET_S,
    bootstrap_room,
    room_app,
    room_seats,
    seat_story,
    wait_for,
    wait_snippet,
    wait_thread_by_channel,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

REFERENCE = "Court order 77/2026"
DONE_TAKEDOWN = "Taken down. A tombstone is served in its place and the author can appeal."
WORKING = "Submitting…"

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # admin-nest page render after the nav patch
_VERDICT_BUDGET_S = 60.0  # one WS-RPC round-trip + the status repaint
_REPAINT_BUDGET_S = 30.0  # a card or dialog following the state it paints


def _poll(check, budget_s, tag):
    """Deadline-poll ``check`` until truthy; the failure names the budget."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    raise AssertionError(f"{tag}: not reached within {budget_s}s")


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("feed-read")
def test_a_taken_down_post_shows_the_notice_in_a_quote_and_when_opened(admin_app):
    """post → quote + repost (both embeds show the words) → takedown through
    the console → barrier compose → both embeds show the tombstone → opening
    the repost row shows the tombstone as the post's body, no error."""
    app = admin_app
    feed = app.feed
    token = uuid.uuid4().hex[:10]
    target_text = f"takedown notice target {token}"
    tombstone = S.moderation.legal_takedown.tombstone(reference=REFERENCE)

    feed.navigate()
    feed.create_post(target_text)
    row = feed.wait_for_post_state_by_text(target_text)
    assert row is not None and row.get("post_id"), (
        f"the target post must land in feed state; error={app.error_text()!r}"
    )
    target_id = row["post_id"]

    # A quote and a repost compose without reloading the window (feed.md
    # § Interaction bar), so a marker compose is the barrier that brings both
    # rows in.
    feed.quote_post(feed.post_index_by_id(target_id))
    _poll(
        lambda: (feed.post_state_by_id(target_id) or {}).get("quote_count") == 1,
        _REPAINT_BUDGET_S,
        "the quote of the target is composed",
    )
    feed.repost_post(feed.post_index_by_id(target_id))
    feed.create_post(f"takedown notice repost barrier {token}")
    repost_row = feed.wait_for_repost_row_by_target(target_id, timeout=_REPAINT_BUDGET_S)
    assert repost_row is not None, (
        f"the repost row must render after the barrier; error={app.error_text()!r}"
    )
    _poll(
        lambda: feed.quote_row_state_by_target(target_id) is not None,
        _REPAINT_BUDGET_S,
        "the quote of the target lands in the feed",
    )
    quote_id = feed.quote_row_state_by_target(target_id)["post_id"]
    repost_id = repost_row["post_id"]

    def embed_of(post_id):
        at = feed.post_index_by_id(post_id)
        return None if at < 0 else feed.quoted_post_text(at)

    # The premise: this device has already shown the words in both embeds.
    for post_id, what in ((quote_id, "quote"), (repost_id, "repost row")):
        wait_until(
            lambda: target_text in (embed_of(post_id) or ""),
            _REPAINT_BUDGET_S,
            diagnose=lambda: f"the {what}'s embed={embed_of(post_id)!r}",
        )

    # --- The takedown, through the console (witnessed in full by
    # test_admin_legal_takedown.py — driven here only far enough to take
    # the post down).
    app.admin.navigate_nest()
    _poll(
        lambda: app.driver.count("admin-nest-takedown-content-id-input") > 0,
        _PAGE_BUDGET_S,
        "admin-nest renders the takedown console",
    )
    app.admin.takedown_fill(target_id, REFERENCE)
    _poll(
        lambda: app.driver.is_enabled("admin-nest-takedown-button"),
        _PAGE_BUDGET_S,
        "a well-formed takedown becomes armable",
    )
    app.admin.takedown_arm()
    app.admin.takedown_confirm()
    _poll(
        lambda: app.driver.count("admin-nest-takedown-status") > 0
        and app.admin.takedown_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        "the takedown reports its verdict",
    )
    assert app.admin.takedown_status_text() == DONE_TAKEDOWN, (
        f"the takedown must succeed, got {app.admin.takedown_status_text()!r}"
    )

    # The barrier: a compose ends in a feed re-pull, so once its marker
    # renders, the feed has read what the nest serves now.
    feed.navigate()
    feed.create_post(f"takedown notice barrier {token}")
    _poll(
        lambda: feed.post_state_by_id(target_id) is None,
        _REPAINT_BUDGET_S,
        "the taken-down post leaves the feed",
    )

    for post_id, what in ((quote_id, "quote"), (repost_id, "repost row")):
        wait_until(
            lambda: embed_of(post_id) == tombstone,
            _REPAINT_BUDGET_S,
            diagnose=lambda: (
                f"the {what}'s embed={embed_of(post_id)!r} (want {tombstone!r}) "
                f"error={app.error_text()!r}"
            ),
        )

    # Opening the repost row opens its ORIGINAL (feed.md § Interaction bar →
    # Repost) — the taken-down post, through the by-id read that sees the
    # takedown marker.
    feed.open_post_detail(feed.post_index_by_id(repost_id))
    _poll(feed.post_detail_visible, _REPAINT_BUDGET_S, "the post detail opens")
    wait_until(
        lambda: feed.post_detail_body() == tombstone,
        _REPAINT_BUDGET_S,
        diagnose=lambda: f"detail body={feed.post_detail_body()!r} (want {tombstone!r})",
    )
    assert target_text not in feed.post_detail_body()
    assert not app.has_error(), (
        f"the notice stands in the body, never a page error: {app.error_text()!r}"
    )


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


def _painted(seat, token):
    """Whether ``token`` has reached the seat's terminal. The tombstone is
    chrome — presentation with no test id, like the quoted post's — so the
    witness reads what the terminal was sent; the tui driver IS that terminal
    (``drivers/tui.py``, ``frame_stream_path``). ``token`` must be space-free:
    the renderer skips unchanged cells, so a phrase need not reach the stream
    as one run (``test_tui_render_control_char_strip.py``)."""
    with open(seat.app.driver.frame_stream_path(), "rb") as stream:
        return token.encode() in stream.read()


@pytest.mark.real_conversations
@pytest.mark.feature("feed-read", "admin-nest")
def test_a_message_taken_down_from_a_report_shows_the_notice_to_a_member_who_reads_it_later(
    room_seats, nest_instance
):
    """A room of three; the late reader closes their app → the author writes
    the room → a member reports the message from its menu → the admin opens
    the takedown from the queue row: the console comes pre-filled with the
    message's id as a conversation takedown, still unarmable without a
    citation → cite, arm (the confirm names the kind, the id and the
    citation), confirm → the late reader opens their app and the thread shows
    the notice where the message would be, never its words, never an error."""
    author, reporter, reader = room_seats
    token = uuid.uuid4().hex[:10]
    words = f"takedownbody{token}"
    reference = f"CourtOrder-{token}"
    note = f"takedown note {token}"

    # ── The room, with the late reader in it ──
    group_id, channel = bootstrap_room(author, reporter, reader)
    wait_thread_by_channel(reader, channel, what="the reader's device applying the Welcome")
    wait_snippet(reader, channel, "hi room", what="the reader reading the room's first message")

    # ── The reader closes the app. Their device keeps its store (pinned), and
    # its copy of the room on the nest has settled, so the reopened app is the
    # same device resuming from where it stopped — before the message. ──
    if not reader.app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot keep the reader's store across a relaunch, so the "
            "reopened app would be a fresh device, not the one that was away"
        )

    def _reader_client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=reader.actor["actor_id_bytes"],
            signing_key=bytes(reader.actor["signing_key"]),
        )

    wait_replica_settled(_reader_client, f"history/{channel}")
    wait_replica_settled(_reader_client, "provider")
    reader.app.driver.quit_app()

    # ── The author writes the room; the member reports it from its menu ──
    author.app.conversations.real_send(group_id, words)
    received = wait_snippet(reporter, channel, words, what="the member receiving the message")
    reporter.app.conversations.open_thread_by_id(received.thread_id)
    reporter.app.conversations.open_message_actions(message_index=_bubble_index(reporter, words))
    reporter.app.report.open_from_message_menu()
    reporter.app.report.choose_reason("other")
    reporter.app.report.fill_note(note)
    _poll(reporter.app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    reporter.app.report.submit()
    _poll(reporter.app.report.has_status, _VERDICT_BUDGET_S, "the report is acknowledged")

    # ── The admin: open takedown pre-fills the console's conversation half ──
    admin = author.app
    login_as_nest_admin(admin, nest_instance, nest_instance["url"])
    admin.admin.navigate_nest()
    _poll(
        lambda: admin.driver.count("admin-nest-reports-section") > 0
        and admin.admin.report_index(note) >= 0,
        _PAGE_BUDGET_S,
        "the message report waits in the admin queue",
    )
    rows = admin.admin.report_rows()
    row = admin.admin.report_index(note)
    # An account report paints no open-takedown button, so this row's button
    # index skips the account rows above it (test_abuse_reporting.py's idiom).
    admin.admin.report_open_takedown(sum(1 for r in rows[:row] if " actor " not in r))
    _poll(
        lambda: len(admin.admin.takedown_content_id()) == 64,
        _PAGE_BUDGET_S,
        "open takedown pre-fills the console with the message's record id",
    )
    content_id = admin.admin.takedown_content_id()
    assert all(c in "0123456789abcdef" for c in content_id), (
        f"the pre-fill is the message's 32-byte record id in hex, got {content_id!r}"
    )
    assert not admin.driver.is_enabled("admin-nest-takedown-button"), (
        "a pre-filled takedown must still require a legal reference"
    )
    admin.admin.takedown_cite(reference)
    _poll(
        lambda: admin.driver.is_enabled("admin-nest-takedown-button"),
        _PAGE_BUDGET_S,
        "a cited takedown becomes armable",
    )
    admin.admin.takedown_arm()
    summary = admin.admin.takedown_confirm_summary()
    assert f"conversation {content_id}" in summary, (
        f"the confirm must name a CONVERSATION takedown of the reported message, got {summary!r}"
    )
    assert reference in summary, f"the confirm must name the citation, got {summary!r}"
    admin.admin.takedown_confirm()
    _poll(
        lambda: admin.driver.count("admin-nest-takedown-status") > 0
        and admin.admin.takedown_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        "the takedown reports its verdict",
    )
    assert admin.admin.takedown_status_text() == DONE_TAKEDOWN, (
        f"the takedown must succeed, got {admin.admin.takedown_status_text()!r} "
        f"(error-message: {admin.error_text() if admin.has_error() else 'none'})"
    )

    # ── The reader comes back: their first fetch of the message follows the
    # takedown, so the thread shows the notice in its place ──
    reader.app.driver.hard_reload()
    wait_until_online(reader.app.driver)
    reader.app.conversations.enable_real_faunamls()
    wait_thread_by_channel(reader, channel, what="the reopened reader's room")
    reader.app.conversations.navigate()
    reader.app.conversations.open_thread_by_channel(channel)
    wait_for(
        "the notice painting where the message would be",
        lambda: _painted(reader, reference),
        bool,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(reader),
    )
    # The barrier above makes these non-vacuous: the thread painted after the
    # walk that brought the record in, and the words came with it nowhere.
    assert not _painted(reader, words), "the withheld words must never reach the reader"
    painted = [
        reader.app.driver.get_text("dm-message-text", index=i)
        for i in range(reader.app.driver.count("dm-message-text"))
    ]
    assert not any(words in t for t in painted), f"no bubble carries the words: {painted!r}"
    assert not reader.app.has_error(), (
        f"the notice stands in the bubble, never a page error: {reader.app.error_text()!r}"
    )
