"""tier_3 e2e: user-initiated reporting, end to end through the app UI.

``moderation.md`` § User-initiated reporting (ratified 2026-09-25): a user
reports someone else's post to their nest's admins, the admins hear a doorbell,
the report waits in the ``admin-nest`` reports queue beside the legal-takedown
console, *open takedown* pre-fills that console (its own guards still standing),
the admin records an outcome, and the reporter is told the outcome — nothing
more. Plus the two personal filters the section rules beside it (§ Corollary):
what a reporter reported stops painting for them ("You reported this"), and
blocking an author hides their posts from the blocker's feed until unblocked.

Three journeys:

1. **The loop** (``app``, three identities switched through the shared session
   helpers): a fresh reporter reports a headless author's post (the verb is
   gated ``!is_own``), the nest admin hears the doorbell and works the queue,
   and the reporter comes back for the outcome. Never the admin as reporter:
   the nest rings no doorbell for a report its reader filed.
2. **The ledger** (``logged_in_app``): a profile (account) report shows on the
   reporter's Moderation page as open, and withdrawing it flips it to
   withdrawn.
3. **Block hides** (``logged_in_app``): block an author from their profile →
   their post leaves the feed; unblock → it returns.

Then the sheet's own promises, one journey each: a received message reported
from its menu, its text reaching the admins only when the excerpt box is ticked
(two seats, ``room_seat_pair``), and the same box on a restricted post; block in
the same step; no verb on your own post, message or profile; and an account
report hiding everything that account wrote.

Mutations are UI-only (convention 8) save the OTHER actor's post, which is that
actor's own act through the nest API — the seed, not the journey. Waits are
named budgets over deadline polls (convention 14); every feed-exclusion assert
rides a re-selected feed, the one reload a user can drive.

App arm: **tui** (the lead app — ``testing.md`` § Default app and nest mode),
then **web**, then **macos** and **ios** (one shared FaunaKit paint), then
**windows**, **android** and **linux**.
"""

import re
import time
import uuid

import pytest

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.app_surface import app_name, skip_unbuilt
from helpers.e2e_session import E2E_LOGIN_DEVICE_ID, login_as
from helpers.mail_dedicated_nest import login_as_nest_admin
from i18n.strings import S
from helpers.room_seats import (  # noqa: F401 — room_app/room_seat_pair are pytest fixtures adopted by import
    MESSAGE_BUDGET_S,
    room_app,
    room_seat_pair,
    seat_story,
    wait_for,
)
from tests.api import ws_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
    pytest.mark.linux,
]

_BUILT_APPS = ("tui", "web", "macos", "ios", "windows", "android", "linux")

# The shared words every app renders (i18n `moderation.report.*`,
# `notifications.row_abuse_report_*`, en.yaml).
HIDDEN_PLACEHOLDER = "You reported this"
DOORBELL = "A report is waiting in the reports queue"
REVIEWED = "Your report was reviewed"
ACK_LOCAL = "Report sent to the admins of {nest}."
# The consequence the excerpt checkbox's label must name (moderation.md § What
# a report carries; `moderation.report.include_text_label`).
INCLUDE_TEXT_CONSEQUENCE = "the admins will be able to read it"

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # a page render after a nav
_SEND_BUDGET_S = 60.0     # one WS-RPC round-trip + the repaint
_FEED_BUDGET_S = 30.0     # a re-selected feed reflecting the change
_NOTIFY_BUDGET_S = 60.0   # a notification row reaching the list


def _poll(check, budget_s, tag):
    """Deadline-poll ``check`` until truthy; the failure names the budget."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    raise AssertionError(f"{tag}: not reached within {budget_s}s")


def _require_built(app, surface):
    if app_name(app.driver) not in _BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface=surface,
            detail="user-initiated reporting (moderation.md § User-initiated "
            "reporting) is built on tui first",
            tracked="",
        )


def _other_actor(nest_instance):
    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    return other, ApiActor(
        nest_instance["url"], other["token"], other["actor_id_hex"],
        bytes(other["signing_key"]),
    )


def _seed_post(app, actor, text):
    """The OTHER actor posts; the app re-selects its feed and finds the card."""
    post_id = actor.post_to_feed("", text)
    app.feed.navigate()
    app.feed.open_feed("General")
    assert app.feed.wait_for_post_text(text, timeout_s=_FEED_BUDGET_S), (
        f"the OTHER actor's post should appear in the feed; error={app.error_text()!r}"
    )
    return post_id


def _notification_rows(app):
    return [
        app.driver.get_text("notification-item", index=i)
        for i in range(app.notifications.notification_count())
    ]


def _wait_for_notification(app, needle, tag):
    def seen():
        app.notifications.navigate()
        return any(needle in row for row in _notification_rows(app))

    _poll(seen, _NOTIFY_BUDGET_S, tag)


def _assert_acknowledged_by_own_nest(app, nest):
    """Wait for `report-status` and assert it names the reporter's own nest —
    the whole local acknowledgement (`moderation.report.sent_local`), not just
    its first words. The name is the nest's handle domain; ``fauna.nest.info``
    publishes it once the nest has one, and the line must then carry exactly
    that name."""
    _poll(app.report.has_status, _SEND_BUDGET_S, "the report is acknowledged")
    ack = app.report.status_text()
    prefix, suffix = "Report sent to the admins of ", "."
    named = ack[len(prefix):-len(suffix)] if ack.startswith(prefix) and ack.endswith(suffix) else ""
    assert named.strip(), (
        f"the acknowledgement must name the admins' nest ({ACK_LOCAL!r}), got {ack!r} "
        f"(error-message: {app.error_text() if app.has_error() else 'none'})"
    )
    domain = ws_api.nest_info(nest["port"]).get("domain")
    if domain and domain != "unknown":
        assert named == domain, (
            f"the acknowledgement must name this nest ({domain!r}), got {ack!r}"
        )


def _login_reporter(app, nest_instance, reporter, request=None):
    """Pass ``request`` when the journey uploads (a sold post's sealed body):
    web then dials the session nest through ``spa_url``, the CORS-bearing proxy
    the blob POST needs (`helpers.e2e_session.resolve_node_url`)."""
    login_as(
        app,
        nest_instance,
        reporter,
        handle=reporter["handle"],
        device_id=E2E_LOGIN_DEVICE_ID,
        request=request,
        spa_url_fixture="spa_url" if request is not None else None,
        wait_for_id="feed-tab",
    )


@pytest.mark.feature("report-abuse", "admin-nest")
def test_a_report_reaches_the_admin_queue_and_the_outcome_returns(app, nest_instance):
    """report → hidden for the reporter → the admins' doorbell → queue row →
    open-takedown pre-fill (citation still required) → mark acted → the row
    leaves the queue → the reporter's outcome notification → the ledger reads
    resolved.

    Three identities on the one driver, switched through the shared session
    helpers: a fresh reporter, the post's author (headless), and the nest
    admin. The reporter is never the admin — the nest deliberately rings no
    doorbell for a report the admin filed themselves."""
    _require_built(app, "report-sheet")
    token = uuid.uuid4().hex[:10]
    text = f"report target {token}"
    note = f"note {token}"
    reporter, _ = _other_actor(nest_instance)
    _, author = _other_actor(nest_instance)
    _login_reporter(app, nest_instance, reporter)
    post_id = _seed_post(app, author, text)

    # The sheet, through the post's own ⋯ verb.
    idx = app.feed.post_index_by_text(text)
    assert idx >= 0
    app.report.open_from_post(app.feed, idx)
    assert not app.report.submit_enabled(), (
        "a report with no reason must not be sendable"
    )
    assert not app.report.has_include_text(), (
        "a public post needs no excerpt — the checkbox renders only for sealed content"
    )
    app.report.choose_reason("harassment")
    app.report.fill_note(note)
    _poll(app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    app.report.submit()
    _assert_acknowledged_by_own_nest(app, nest_instance)
    assert not app.report.is_open(), "a landed report closes the sheet"

    # The reporter-side hide: the post stops painting for its reporter.
    assert not app.has_error(), (
        f"the block/hide follow-ups must land beside the report: {app.error_text()!r}"
    )
    assert app.feed.wait_for_post_text_absent(text, _FEED_BUDGET_S), (
        "a reported post must stop painting for the reporter; "
        f"error-message: {app.error_text() if app.has_error() else 'none'}"
    )
    assert app.driver.count("content-policy-blocked-notice") > 0
    assert any(
        HIDDEN_PLACEHOLDER in app.driver.get_text("content-policy-blocked-notice", index=i)
        for i in range(app.driver.count("content-policy-blocked-notice"))
    ), "the placeholder names the reporter's own act"

    # The admins' doorbell — rung for every admin but the reporter.
    login_as_nest_admin(app, nest_instance, nest_instance["url"])
    _wait_for_notification(app, DOORBELL, "the admin doorbell rings")

    # The queue row, carrying the reporter's note.
    app.admin.navigate_nest()
    _poll(
        lambda: app.driver.count("admin-nest-reports-section") > 0
        and app.admin.report_index(post_id) >= 0,
        _PAGE_BUDGET_S,
        "the report waits in the admin queue",
    )
    rows = app.admin.report_rows()
    row = app.admin.report_index(post_id)
    assert note in rows[row], "the queue row carries the note"

    # Open takedown pre-fills the console — and the console's own guard stands:
    # no citation, no arm. An account report paints no open-takedown button,
    # so this row's button index skips the account rows above it.
    takedown_index = sum(1 for r in rows[:row] if " actor " not in r)
    app.admin.report_open_takedown(takedown_index)
    _poll(
        lambda: app.admin.takedown_content_id() == post_id,
        _PAGE_BUDGET_S,
        "open takedown pre-fills the console with the reported post",
    )
    assert not app.driver.is_enabled("admin-nest-takedown-button"), (
        "a pre-filled takedown must still require a legal reference"
    )

    # A record, not an action.
    app.admin.report_mark_acted(row)
    _poll(
        lambda: app.admin.report_index(post_id) < 0,
        _SEND_BUDGET_S,
        "a resolved report leaves the queue",
    )

    # The reporter hears the outcome — only the outcome.
    _login_reporter(app, nest_instance, reporter)
    _wait_for_notification(app, REVIEWED, "the reporter is told the outcome")

    # …and the ledger reads it the same way.
    app.moderation.navigate()
    _poll(
        lambda: any(
            "Resolved" in r and "Acted on" in r and "Harassment" in r
            for r in app.moderation.report_rows()
        ),
        _PAGE_BUDGET_S,
        "the ledger shows the report resolved as acted on",
    )


_FEDERATED_APPS = ("tui", "linux", "web")

# A discovery poll fires the moment its feed is created; the budget covers the
# federation channel's first dial to a TLS peer plus the index write.
_DISCOVERY_BUDGET_S = 90.0
# The forward's one inline try waits 4 s; outcome and withdraw calls are nudged
# straight out of the outbox. Generous ceilings over both.
_FEDERATION_BUDGET_S = 90.0


def _require_federated_built(app):
    if app_name(app.driver) not in _FEDERATED_APPS:
        skip_unbuilt(
            app.driver,
            surface="report-sheet",
            detail="the UI-driven two-nest leg of user-initiated reporting "
            "(moderation.md § User-initiated reporting → Routing) is driven on "
            "tui first",
            tracked="",
        )


def _wait_for_post_id(app, post_id, tag):
    """Re-enter the General feed until the card for ``post_id`` is in the
    window — the read a user drives by coming back to the feed. A discovery
    stub carries no body, so the card is found by its id, not its text."""
    def present():
        app.feed.navigate()
        app.feed.open_feed("General")
        return app.feed.post_index_by_id(post_id) >= 0

    _poll(present, _DISCOVERY_BUDGET_S, tag)


def _report_post(app, post_id, reason):
    """Report the post ``post_id`` through its ⋯ verb; return the acknowledgement."""
    idx = app.feed.post_index_by_id(post_id)
    assert idx >= 0, f"the post {post_id} must be in the feed window to report it"
    app.report.open_from_post(app.feed, idx)
    app.report.choose_reason(reason)
    _poll(app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    app.report.submit()
    _poll(app.report.has_status, _SEND_BUDGET_S, "the report is acknowledged")
    return app.report.status_text()


@pytest.mark.feature("report-abuse")
def test_a_report_on_a_foreign_post_reaches_its_home_nest_and_the_outcome_returns(
    app, report_origin_nest, cross_nest_foreign
):
    """The federation triad through the app UI (`moderation.md` § Routing):
    a user on nest O reports two posts whose author lives on nest H → each
    acknowledgement names both nests and that the reporter's name stayed
    home → the reporter withdraws the second → H's admin sees the first in
    the queue as "a user of O", no reporter named, and never the withdrawn
    one → marks it acted → the reporter on O is told the outcome.

    The posts reach O the one way a foreign post gets an origin there: a
    discovery feed on O seeded with H indexes them as stubs (no body) that
    name the nest they came from. The author's posts and that feed are the
    seed — other actors' own acts through the nest API (convention 8); every
    reporter and admin step is the app.

    Reads ``peer_url`` (H is DIALLED by O, and O by H): a nest-dials-nest
    journey, standalone only (`testing.md` § Default app and nest mode,
    ruling (2))."""
    _require_federated_built(app)
    origin, home = report_origin_nest, cross_nest_foreign
    token = uuid.uuid4().hex[:10]

    # ── Seed: an author on H posts twice; a user on O follows H by discovery ──
    author, author_api = _other_actor(home)
    kept_id = author_api.post_to_feed("", f"federated report target {token} one")
    withdrawn_id = author_api.post_to_feed("", f"federated report target {token} two")
    follower, _ = _other_actor(origin)
    ws_api.create_feed(
        origin["port"],
        follower,
        f"discovery {token}",
        rules=[{"BodyContains": {"terms": [token]}}],
        scope="discovery",
        contributor_seeds=[home["peer_url"]],
    )

    # ── The reporter on O finds both stubs in their feed and reports them ──
    reporter, reporter_api = _other_actor(origin)
    # A dedicated nest provisions no feed: the account's default "General"
    # (empty rules — every post this nest holds) is account setup, as
    # `_make_user` makes it, not a step of the journey.
    reporter_api.create_feed("General")
    _login_reporter(app, origin, reporter)
    _wait_for_post_id(app, kept_id, "the discovery poll indexes the first foreign post on O")
    _wait_for_post_id(app, withdrawn_id, "the discovery poll indexes the second foreign post on O")

    for post_id, reason in ((kept_id, "harassment"), (withdrawn_id, "spam")):
        ack = _report_post(app, post_id, reason)
        assert origin["authority"] in ack and home["authority"] in ack and "without your name" in ack, (
            f"the acknowledgement of a report on a foreign post must name the reporter's "
            f"nest ({origin['authority']}) and the author's home ({home['authority']}) and "
            f"say the reporter's name stayed home, got {ack!r} "
            f"(error-message: {app.error_text() if app.has_error() else 'none'})"
        )

    # ── The withdraw: the second report leaves the ledger as withdrawn ──
    app.moderation.navigate()
    _poll(
        lambda: any("Spam" in r and "Open" in r for r in app.moderation.report_rows()),
        _PAGE_BUDGET_S,
        "the ledger lists the open forwarded spam report",
    )
    # The ledger lists newest first, and the spam report was filed last.
    app.moderation.withdraw_report(0)
    _poll(
        lambda: any("Spam" in r and "Withdrawn" in r for r in app.moderation.report_rows()),
        _SEND_BUDGET_S,
        "the withdrawn forwarded report reads withdrawn",
    )

    # ── H's admin: the forwarded copy, reporter-anonymous; never the withdrawn one ──
    login_as_nest_admin(app, home, home["url"])
    app.admin.navigate_nest()
    _poll(
        lambda: app.admin.report_index(kept_id) >= 0
        and app.admin.report_index(withdrawn_id) < 0,
        _FEDERATION_BUDGET_S,
        "the home nest's queue holds the forwarded report and not the withdrawn one",
    )
    rows = app.admin.report_rows()
    row = app.admin.report_index(kept_id)
    assert f"a user of {origin['authority']}" in rows[row], (
        f"a forwarded report names its origin nest, got {rows[row]!r}"
    )
    for trace in (reporter["handle"], reporter["actor_id_hex"][:8]):
        assert trace not in rows[row], (
            f"a forwarded report must not name its reporter ({trace!r}), got {rows[row]!r}"
        )

    app.admin.report_mark_acted(row)
    _poll(
        lambda: app.admin.report_index(kept_id) < 0,
        _SEND_BUDGET_S,
        "a resolved forwarded report leaves the home nest's queue",
    )

    # ── Back on O: the outcome came home ──
    _login_reporter(app, origin, reporter)
    _wait_for_notification(app, REVIEWED, "the reporter on O is told the home nest's outcome")
    app.moderation.navigate()
    _poll(
        lambda: any(
            "Resolved" in r and "Acted on" in r and "Harassment" in r
            for r in app.moderation.report_rows()
        ),
        _FEDERATION_BUDGET_S,
        "the reporter's ledger shows the forwarded report resolved as acted on",
    )


@pytest.mark.feature("moderation-queue", "report-abuse")
def test_an_account_report_shows_in_the_ledger_and_can_be_withdrawn(
    logged_in_app, nest_instance
):
    """profile → report the account → the Moderation ledger lists it open →
    withdraw → it reads withdrawn and offers no second withdraw."""
    app = logged_in_app
    _require_built(app, "profile-report-button")
    app.profile.require_state_protocol_actor_nav_supported()
    other, _ = _other_actor(nest_instance)

    app.profile.navigate_to_actor(other["actor_id_hex"])
    app.report.open_from_profile()
    assert not app.report.has_include_text(), "an account has no text to attach"
    app.report.choose_reason("impersonation")
    _poll(app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    app.report.submit()
    _poll(app.report.has_status, _SEND_BUDGET_S, "the report is acknowledged")

    short = other["actor_id_hex"][:8]

    def ledger_row():
        return next((r for r in app.moderation.report_rows() if short in r), None)

    app.moderation.navigate()
    _poll(
        lambda: (ledger_row() or "").find("Open") >= 0,
        _PAGE_BUDGET_S,
        "the ledger lists the open account report",
    )
    before = app.moderation.withdraw_count()
    assert before > 0
    # The newest report is this one — the ledger lists newest first, and each
    # open row carries its own withdraw.
    app.moderation.withdraw_report(0)
    _poll(
        lambda: "Withdrawn" in (ledger_row() or ""),
        _SEND_BUDGET_S,
        "a withdrawn report reads withdrawn",
    )
    assert app.moderation.withdraw_count() == before - 1, (
        "a withdrawn report offers no second withdraw"
    )


@pytest.mark.feature("feed-read")
def test_blocking_an_author_hides_their_posts_until_unblocked(logged_in_app, nest_instance):
    """block from the profile → the author's post leaves the blocker's feed →
    unblock → it returns (moderation.md § Corollary — block also hides)."""
    app = logged_in_app
    _require_built(app, "profile-block-button")
    app.profile.require_state_protocol_actor_nav_supported()
    other, actor = _other_actor(nest_instance)
    text = f"blocked author's post {uuid.uuid4().hex[:10]}"
    _seed_post(app, actor, text)

    app.profile.navigate_to_actor(other["actor_id_hex"])
    app.profile.block()
    _poll(
        lambda: app.profile.block_label() != "Block",
        _SEND_BUDGET_S,
        "the block lands",
    )
    # The feed is already selected from the seed: re-ENTER it (a page entry
    # re-queries the current selection on every app) rather than re-click the
    # selected row, a gesture linux answers with no reload.
    app.feed.return_to_feed_page()
    assert app.feed.wait_for_post_text_absent(text, _FEED_BUDGET_S), (
        "a blocked author's post must leave the blocker's feed"
    )

    app.profile.navigate_to_actor(other["actor_id_hex"])
    app.profile.block()
    _poll(
        lambda: app.profile.block_label() == "Block",
        _SEND_BUDGET_S,
        "the unblock lands",
    )
    app.feed.return_to_feed_page()
    assert app.feed.wait_for_post_text(text, timeout_s=_FEED_BUDGET_S), (
        "unblocking restores the author's post on the next read"
    )


def _open_sheet_on_post(app, text):
    idx = app.feed.post_index_by_text(text)
    assert idx >= 0, f"the post {text!r} must be in the feed window to report it"
    app.report.open_from_post(app.feed, idx)


def _assert_excerpt_box_offered(app, what):
    """The excerpt checkbox is offered for a sealed subject, starts unticked,
    and its label names the consequence."""
    assert app.report.has_include_text(), (
        f"{what} is sealed — the sheet must offer to attach its text"
    )
    assert not app.report.include_text_checked(), (
        "attaching the text is opt-in per report: the box starts unticked"
    )
    label = app.report.include_text_label()
    assert INCLUDE_TEXT_CONSEQUENCE in label, (
        f"the checkbox's label must name the consequence "
        f"({INCLUDE_TEXT_CONSEQUENCE!r}), got {label!r}"
    )


def _wait_for_queue_rows(app, needles, budget_s, tag):
    """As the admin, open `admin-nest` and wait until a queue row carries each
    needle; return ``{needle: row text}``."""
    app.admin.navigate_nest()
    _poll(
        lambda: all(app.admin.report_index(n) >= 0 for n in needles),
        budget_s,
        tag,
    )
    rows = app.admin.report_rows()
    return {n: rows[app.admin.report_index(n)] for n in needles}


def _bubble_index(seat, needle):
    """The index of the live bubble carrying ``needle`` — also its
    `dm-message-actions-button` index (a bubble with no text paints no ⋯)."""
    d = seat.app.driver
    texts = wait_for(
        f"a bubble carrying {needle!r} painting",
        lambda: [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))],
        lambda ts: any(needle in t for t in ts),
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )
    return next(i for i, t in enumerate(texts) if needle in t)


def _menu_painted(seat):
    """Wait for the open message menu to paint its reaction row, so an absent
    verb read after it is absent, not late."""
    wait_for(
        "the message menu painting its reaction row",
        lambda: seat.app.driver.count("dm-reaction-option"),
        lambda n: n > 0,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat.app.driver.diagnose("dm-message-actions-menu"),
    )


def _fauna_thread_with(seat, needle, what):
    return wait_for(
        what,
        lambda: next(
            (
                t
                for t in seat.app.conversations.list_threads()
                if t.rail == "FaunaMls" and needle in (t.snippet or "")
            ),
            None,
        ),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(seat),
    )


@pytest.mark.real_conversations
@pytest.mark.feature("report-abuse")
def test_a_received_message_is_reported_from_its_menu_and_its_text_goes_only_if_ticked(
    room_seat_pair, nest_instance
):
    """Two seats on one nest: the sender writes the reporter two messages.
    The sender's own bubble offers no report verb; the reporter reports both
    from each message's ⋯ menu — the sheet offers the excerpt box (unticked,
    naming the consequence) — ticking it on the first and not on the second.
    The admin's queue then carries the first message's text and not the
    second's (moderation.md § What a report carries).

    The messages are the sender's own acts through the real-wire commands
    (convention 8 carve-out (b), as ``bootstrap_room`` seeds a room); every
    reporter and admin step is the app."""
    sender, reporter = room_seat_pair
    _require_built(reporter.app, "dm-message-report-button")
    token = uuid.uuid4().hex[:10]
    attached = f"attached message {token}"
    withheld = f"withheld message {token}"
    note_attached, note_withheld = f"note-a {token}", f"note-w {token}"

    # ── Seed: the sender writes the reporter twice ──
    sender.app.conversations.real_resolve_send_new(reporter.actor_hex, attached)
    sent = _fauna_thread_with(sender, attached, "the sender's 1:1 carrying the first message")
    sender.app.conversations.real_send(sent.thread_id, withheld)

    # ── Outcome 6: your own message offers no report verb ──
    sender.app.conversations.open_thread_by_id(sent.thread_id)
    sender.app.conversations.open_message_actions(message_index=_bubble_index(sender, attached))
    _menu_painted(sender)
    assert sender.app.driver.count("dm-message-report-button") == 0, (
        "reporting is not offered on your own message: "
        f"{sender.app.driver.diagnose('dm-message-report-button')}"
    )

    # ── The reporter reports each message from its own menu ──
    received = _fauna_thread_with(
        reporter, withheld, "the reporter's thread carrying the second message"
    )
    reporter.app.conversations.open_thread_by_id(received.thread_id)
    for needle, note, attach in ((attached, note_attached, True), (withheld, note_withheld, False)):
        reporter.app.conversations.open_message_actions(
            message_index=_bubble_index(reporter, needle)
        )
        reporter.app.report.open_from_message_menu()
        _assert_excerpt_box_offered(reporter.app, "a received message")
        reporter.app.report.choose_reason("harassment")
        reporter.app.report.fill_note(note)
        if attach:
            reporter.app.report.tick_include_text()
            _poll(reporter.app.report.include_text_checked, _PAGE_BUDGET_S, "the box ticks")
        _poll(reporter.app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
        reporter.app.report.submit()
        _assert_acknowledged_by_own_nest(reporter.app, nest_instance)

    # ── The admin's queue: the text only where it was attached ──
    login_as_nest_admin(sender.app, nest_instance, nest_instance["url"])
    rows = _wait_for_queue_rows(
        sender.app, (note_attached, note_withheld), _PAGE_BUDGET_S,
        "both message reports wait in the admin queue",
    )
    assert attached in rows[note_attached], (
        f"a ticked excerpt reaches the admins: {rows[note_attached]!r}"
    )
    assert withheld not in rows[note_withheld], (
        f"an unticked message's text must not reach the admins: {rows[note_withheld]!r}"
    )


@pytest.mark.feature("report-abuse", "admin-nest")
def test_a_restricted_posts_text_reaches_the_admins_when_ticked(app, nest_instance, request):
    """A restricted (sold) post is sealed, so its report offers the excerpt
    box like a message's; ticked, the admin's queue row carries the text the
    reporter holds (moderation.md § What a report carries — "a gated post").

    The author sells the post through their own composer — their act, in the
    app, under their own login; the reporter and the admin follow on the same
    driver through the shared session helpers."""
    _require_built(app, "report-include-text-checkbox")
    token = uuid.uuid4().hex[:10]
    preview = f"restricted teaser {token}"
    note = f"note {token}"
    author, _ = _other_actor(nest_instance)
    reporter, _ = _other_actor(nest_instance)

    _login_reporter(app, nest_instance, author, request)
    app.feed.navigate()
    app.feed.open_feed("General")
    app.feed.sell_post(f"the sealed body {token}, buyers only.", preview, "$3")

    _login_reporter(app, nest_instance, reporter, request)
    app.feed.navigate()
    app.feed.open_feed("General")
    assert app.feed.wait_for_post_text(preview, timeout_s=_FEED_BUDGET_S), (
        f"the restricted post's card should reach the reporter's feed; error={app.error_text()!r}"
    )
    _open_sheet_on_post(app, preview)
    _assert_excerpt_box_offered(app, "a restricted post")
    app.report.choose_reason("other")
    app.report.fill_note(note)
    app.report.tick_include_text()
    _poll(app.report.include_text_checked, _PAGE_BUDGET_S, "the box ticks")
    _poll(app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    app.report.submit()
    _assert_acknowledged_by_own_nest(app, nest_instance)

    login_as_nest_admin(app, nest_instance, nest_instance["url"])
    rows = _wait_for_queue_rows(
        app, (note,), _PAGE_BUDGET_S, "the restricted post's report waits in the admin queue"
    )
    row = rows[note]
    assert preview in row, f"a ticked excerpt reaches the admins: {row!r}"

    # Every field the queue row owes the admin (moderation.md § App surface):
    # what was reported, why, the note and excerpt (above), where it came from
    # and how old it is.
    assert S.moderation.report.reason.other in row, f"the row names the reason: {row!r}"
    assert " post " in row, f"the row names what kind of thing was reported: {row!r}"
    assert S.admin.nest_page.reports_origin_local(handle=reporter["handle"]) in row, (
        f"the row names where the report came from: {row!r}"
    )
    assert re.search(
        rf"{re.escape(S.time.just_now)}|\d+m ago|\d+h ago", row
    ), f"the row says how old the report is: {row!r}"

    # Dismissed, the report leaves the queue — the other of the two records.
    app.admin.report_dismiss(app.admin.report_index(note))
    _poll(
        lambda: app.admin.report_index(note) < 0,
        _SEND_BUDGET_S,
        "a dismissed report leaves the queue",
    )


@pytest.mark.feature("report-abuse")
def test_reporting_a_post_can_block_its_author_in_the_same_step(logged_in_app, nest_instance):
    """Tick *also block this person* in the sheet → once the report lands the
    author's profile shows them blocked (moderation.md § What a report
    carries — ``block_author`` chains the existing block)."""
    app = logged_in_app
    _require_built(app, "report-block-author-checkbox")
    app.profile.require_state_protocol_actor_nav_supported()
    other, actor = _other_actor(nest_instance)
    text = f"report and block {uuid.uuid4().hex[:10]}"
    _seed_post(app, actor, text)

    _open_sheet_on_post(app, text)
    assert not app.report.block_author_checked(), "blocking is opt-in: the box starts unticked"
    app.report.choose_reason("spam")
    app.report.tick_block_author()
    _poll(app.report.block_author_checked, _PAGE_BUDGET_S, "the block box ticks")
    _poll(app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    app.report.submit()
    _assert_acknowledged_by_own_nest(app, nest_instance)

    app.profile.navigate_to_actor(other["actor_id_hex"])
    _poll(
        lambda: app.profile.block_label() != "Block",
        _SEND_BUDGET_S,
        "the author's profile shows them blocked",
    )


@pytest.mark.feature("report-abuse")
def test_reporting_is_not_offered_on_your_own_post_or_profile(logged_in_app):
    """Your own post's ⋯ menu and your own profile carry no report verb
    (moderation.md § App surface — the verbs are gated ``!is_own``). The own
    message's menu is the message journey's first step."""
    app = logged_in_app
    _require_built(app, "feed-post-report-button")
    text = f"my own post {uuid.uuid4().hex[:10]}"
    app.feed.navigate()
    app.feed.create_post(text)
    idx = app.feed.post_index_by_text(text)
    assert idx >= 0
    # The menu is a descendant of the card on some apps and a detached popup on
    # others, and exactly one is ever open — so its verbs are read unscoped.
    app.feed.open_post_actions(idx)
    _poll(
        lambda: app.driver.count("feed-post-delete-button") > 0,
        _PAGE_BUDGET_S,
        "the own post's menu paints its delete verb",
    )
    assert app.driver.count("feed-post-report-button") == 0, (
        "reporting is not offered on your own post: "
        f"{app.driver.diagnose('feed-post-report-button')}"
    )

    app.profile.navigate()
    assert app.driver.count("profile-report-button") == 0, (
        "reporting is not offered on your own profile: "
        f"{app.driver.diagnose('profile-report-button')}"
    )


@pytest.mark.feature("report-abuse")
def test_reporting_an_account_hides_everything_it_wrote(logged_in_app, nest_instance):
    """Report an account from its profile → every post of that account in the
    reporter's feed is replaced by the "You reported this" note
    (moderation.md § Corollary — an account's actor id on the hidden list
    hides everything that account authored)."""
    app = logged_in_app
    _require_built(app, "profile-report-button")
    app.profile.require_state_protocol_actor_nav_supported()
    other, actor = _other_actor(nest_instance)
    token = uuid.uuid4().hex[:10]
    texts = [f"account post one {token}", f"account post two {token}"]
    for text in texts:
        _seed_post(app, actor, text)

    app.profile.navigate_to_actor(other["actor_id_hex"])
    app.report.open_from_profile()
    app.report.choose_reason("spam")
    _poll(app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    app.report.submit()
    _assert_acknowledged_by_own_nest(app, nest_instance)

    app.feed.return_to_feed_page()
    for text in texts:
        assert app.feed.wait_for_post_text_absent(text, _FEED_BUDGET_S), (
            f"every post of a reported account must stop painting for the reporter: {text!r}; "
            f"error-message: {app.error_text() if app.has_error() else 'none'}"
        )
    notices = [
        app.driver.get_text("content-policy-blocked-notice", index=i)
        for i in range(app.driver.count("content-policy-blocked-notice"))
    ]
    assert sum(HIDDEN_PLACEHOLDER in n for n in notices) >= len(texts), (
        f"each hidden post is replaced by the reporter's note, got {notices!r}"
    )
