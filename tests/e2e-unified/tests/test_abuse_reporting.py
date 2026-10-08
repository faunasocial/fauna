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

Mutations are UI-only (convention 8) save the OTHER actor's post, which is that
actor's own act through the nest API — the seed, not the journey. Waits are
named budgets over deadline polls (convention 14); every feed-exclusion assert
rides a re-selected feed, the one reload a user can drive.

App arm: **tui** (the lead app — ``testing.md`` § Default app and nest mode),
then **web**, then **macos** and **ios** (one shared FaunaKit paint), then
**windows** and **android**. The remaining app (linux) joins its trickle-down by
extending ``_BUILT_APPS`` and carrying its marker.
"""

import time
import uuid

import pytest

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.app_surface import app_name, skip_unbuilt
from helpers.e2e_session import E2E_LOGIN_DEVICE_ID, login_as
from helpers.mail_dedicated_nest import login_as_nest_admin

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
]

_BUILT_APPS = ("tui", "web", "macos", "ios", "windows", "android")

# The shared words every app renders (i18n `moderation.report.*`,
# `notifications.row_abuse_report_*`, en.yaml).
HIDDEN_PLACEHOLDER = "You reported this"
DOORBELL = "A report is waiting in the reports queue"
REVIEWED = "Your report was reviewed"

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


def _login_reporter(app, nest_instance, reporter):
    login_as(
        app,
        nest_instance,
        reporter,
        handle=reporter["handle"],
        device_id=E2E_LOGIN_DEVICE_ID,
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
    _poll(app.report.has_status, _SEND_BUDGET_S, "the report is acknowledged")
    ack = app.report.status_text()
    assert "Report sent" in ack, (
        f"the acknowledgement must name where the report went, got {ack!r} "
        f"(error-message: {app.error_text() if app.has_error() else 'none'})"
    )
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
    app.feed.navigate()
    app.feed.open_feed("General")
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
    app.feed.navigate()
    app.feed.open_feed("General")
    assert app.feed.wait_for_post_text(text, timeout_s=_FEED_BUDGET_S), (
        "unblocking restores the author's post on the next read"
    )
