"""tier_3 e2e: the admin BLOCKS a reported item — and unblocks it — through the app UI.

``moderation.md`` § Admin block of a reported item (owner-ratified 2026-10-16):
a fourth verb on a report-queue row, **Block**, withholds the reported item from
everyone the nest serves but its author, behind the transparency triple — a
notice in the item's place naming the ground and the reason, the author's
appeal, a permanent audit row — and **Unblock** re-serves the same bytes. The
verb exists only on a report row, its confirm names the verb, the item and the
reason before anything dispatches, and the author keeps their own view
(decision 4).

One journey, four identities on the one driver, switched through the shared
session helpers:

author posts → a viewer quotes the post → a reporter reports it → the admin
Blocks it from the queue (the armed confirm names verb + item + reason) → the
report leaves the queue and the item joins the blocked list → the viewer's
quote paints the notice in place of the withheld body → the author's own read
of the same quote keeps the body with the notice beside it → the author's
moderation queue carries the block, and the appeal is recorded → the admin
Unblocks it from the blocked list → the post serves again, the same post id
(the same bytes).

The notice is observed through the quoted-post embed: a blocked post leaves
every feed (``MODERATION_SERVABLE``), so the embed's ``fauna.posts.get``
fallback is the read every viewer makes of it — the takedown tombstone's
posture, on the shared ``admin_block_notice`` face.

Mutations are UI-only (convention 8): the post, the quote, the report, the
block, the appeal and the unblock all go through the app. Waits are named
budgets over deadline polls (convention 14). Queue and list asserts key on this
journey's own post id, never on emptiness — ``admin_app``'s nest is
session-scoped.

App arm: **tui** (the lead app — ``testing.md`` § Default app and nest mode), then
linux, web and android (the batched trickle-down; android's emulator run is host-owed);
windows, macos and ios join their trickle-down by extending ``_BUILT_APPS``.
"""

import time
import uuid

import pytest

from common.auth import create_actor_and_register
from helpers.app_surface import app_name, skip_unbuilt
from helpers.e2e_session import E2E_LOGIN_DEVICE_ID, login_as
from helpers.mail_dedicated_nest import login_as_nest_admin

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
]

_BUILT_APPS = ("tui", "linux", "web", "android")

# The shared words every app renders (i18n, en.yaml): the notice
# (`moderation.admin_block.notice` over `moderation.report.reason.harassment`),
# the queue row's action label, and the appeal's outcome line.
NOTICE = "Blocked by this nest's admin after a report: Harassment"
REASON_WORD = "Harassment"
BLOCKED_ACTION = "Blocked by this nest's admin"
APPEAL_RECORDED = "Appeal recorded. An administrator will review it."
APPEAL_REASON = "This post quotes a public statement; it is not harassment."

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # a page render after a nav
_SEND_BUDGET_S = 60.0     # one WS-RPC round-trip + the repaint
_FEED_BUDGET_S = 30.0     # a feed (re)load reflecting the change
_EMBED_BUDGET_S = 30.0    # the quoted-post embed's lazy fallback resolve


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


def _actor(nest_instance):
    return create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )


def _login(app, nest_instance, actor):
    login_as(
        app,
        nest_instance,
        actor,
        handle=actor["handle"],
        device_id=E2E_LOGIN_DEVICE_ID,
        wait_for_id="feed-tab",
    )


def _general_feed(app):
    app.feed.navigate()
    app.feed.open_feed("General")


def _quote_embed_text(app, target_post_id):
    """The embed text of the card quoting ``target_post_id``, or ``None``."""
    row = app.feed.quote_row_state_by_target(target_post_id)
    if row is None:
        return None
    idx = app.feed.post_index_by_id(row["post_id"])
    if idx < 0:
        return None
    return app.feed.quoted_post_text(idx)


@pytest.mark.feature("admin-nest")
def test_the_admin_blocks_a_reported_post_and_unblocks_it_through_the_app(app, nest_instance):
    """report → Block (named confirm) → blocked list → the viewer's notice →
    the author's own view → appeal → Unblock → the same post serves again."""
    if app_name(app.driver) not in _BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="admin-nest-report-block-button",
            detail="the admin block of a reported item (moderation.md § Admin "
            "block of a reported item) is built on tui first",
            tracked="",
        )

    token = uuid.uuid4().hex[:10]
    target_text = f"block target {token}"
    author, viewer, reporter = (_actor(nest_instance) for _ in range(3))

    # The author posts through the app.
    _login(app, nest_instance, author)
    app.feed.navigate()
    app.feed.create_post(target_text)
    row = app.feed.wait_for_post_state_by_text(target_text, timeout=_FEED_BUDGET_S)
    assert row is not None, f"the target post must land; error={app.error_text()!r}"
    post_id = row["post_id"]

    # A viewer quotes it — the read through which every non-author sees the
    # notice once the post itself has left the feeds.
    _login(app, nest_instance, viewer)
    _general_feed(app)
    assert app.feed.wait_for_post_text(target_text, timeout_s=_FEED_BUDGET_S), (
        f"the viewer must see the author's post; error={app.error_text()!r}"
    )
    app.feed.quote_post(app.feed.post_index_by_id(post_id))
    # The nest-side quote count moves when the quote post lands; a fresh read of
    # the feed then carries the viewer's own quote row on every app.
    assert app.feed.wait_for_interaction_count_by_id(post_id, "quote", 1, timeout=_SEND_BUDGET_S) == 1, (
        f"the viewer's quote must land; error={app.error_text()!r}"
    )
    _general_feed(app)
    _poll(
        lambda: app.feed.quote_row_state_by_target(post_id) is not None,
        _SEND_BUDGET_S,
        "the viewer's quote lands",
        diagnose=lambda: app.error_text() if app.has_error() else "no error",
    )

    # A third user reports it.
    _login(app, nest_instance, reporter)
    _general_feed(app)
    assert app.feed.wait_for_post_text(target_text, timeout_s=_FEED_BUDGET_S)
    # By id, never by text: the viewer's quote card carries the target's text
    # in its embed, and it is the newer card.
    app.report.open_from_post(app.feed, app.feed.post_index_by_id(post_id))
    app.report.choose_reason("harassment")
    _poll(app.report.submit_enabled, _PAGE_BUDGET_S, "a reasoned report becomes sendable")
    app.report.submit()
    _poll(app.report.has_status, _SEND_BUDGET_S, "the report is acknowledged")

    # The admin: Block, from the report row.
    login_as_nest_admin(app, nest_instance, nest_instance["url"])
    app.admin.navigate_nest()
    _poll(
        lambda: app.driver.count("admin-nest-reports-section") > 0
        and app.admin.report_index(post_id) >= 0,
        _PAGE_BUDGET_S,
        "the report waits in the admin queue",
        diagnose=app.admin.report_rows,
    )
    rows = app.admin.report_rows()
    row_index = app.admin.report_index(post_id)
    # An account report paints no block button: index over item rows only.
    block_index = sum(1 for r in rows[:row_index] if " actor " not in r)
    app.admin.report_block(block_index)

    # The confirm names the verb, the item and the reason before anything
    # dispatches — a lever on another user's content is never confirmed blind.
    _poll(
        lambda: app.driver.count("admin-nest-report-block-confirm-button") > 0,
        _PAGE_BUDGET_S,
        "arming paints the confirm",
    )
    summary = app.admin.block_confirm_summary()
    assert summary.startswith("Block"), f"the confirm names the verb: {summary!r}"
    assert post_id in summary, f"the confirm names the item: {summary!r}"
    assert REASON_WORD in summary, f"the confirm names the reason: {summary!r}"
    app.admin.block_confirm()
    assert app.driver.count("admin-nest-report-block-confirm-button") == 0, (
        "the first confirm click must disarm the surface"
    )
    _poll(
        lambda: app.admin.blocked_index(post_id) >= 0,
        _SEND_BUDGET_S,
        "the blocked item joins the blocked list",
        diagnose=lambda: (app.admin.blocked_rows(), app.error_text() if app.has_error() else ""),
    )
    assert REASON_WORD in app.admin.blocked_rows()[app.admin.blocked_index(post_id)]
    assert app.admin.report_index(post_id) < 0, "a blocked item's report leaves the open queue"

    # The viewer: the notice in place of the withheld body, and the post gone
    # from the feed. A fresh sign-in is a fresh read of what the nest serves.
    _login(app, nest_instance, viewer)
    _general_feed(app)
    _poll(
        lambda: _quote_embed_text(app, post_id) == NOTICE,
        _EMBED_BUDGET_S,
        "the viewer's quote paints the notice in place of the body",
        diagnose=lambda: _quote_embed_text(app, post_id),
    )
    assert app.feed.post_state_by_id(post_id) is None, "a blocked post leaves the feed"

    # The author keeps their own view: the body, with the notice beside it.
    _login(app, nest_instance, author)
    _general_feed(app)
    _poll(
        lambda: target_text in (_quote_embed_text(app, post_id) or "")
        and NOTICE in (_quote_embed_text(app, post_id) or ""),
        _EMBED_BUDGET_S,
        "the author's read keeps the body with the notice beside it",
        diagnose=lambda: _quote_embed_text(app, post_id),
    )

    # The author's moderation queue carries the block, and the appeal records.
    app.moderation.navigate()
    _poll(
        lambda: any(
            app.moderation.row_action(i) == BLOCKED_ACTION
            for i in range(app.moderation.correction_count())
        ),
        _PAGE_BUDGET_S,
        "the author's queue carries the block",
        diagnose=lambda: [
            app.moderation.row_action(i) for i in range(app.moderation.correction_count())
        ],
    )
    blocked_row = next(
        i
        for i in range(app.moderation.correction_count())
        if app.moderation.row_action(i) == BLOCKED_ACTION
    )
    # Every nest-issued row offers one appeal handle and local detections none,
    # so the appeal index over server rows is the row's own on a fresh account.
    app.moderation.open_appeal(blocked_row)
    _poll(app.moderation.appeal_form_open, _PAGE_BUDGET_S, "the appeal form opens")
    app.moderation.fill_appeal_reason(APPEAL_REASON)
    _poll(app.moderation.appeal_submit_enabled, _PAGE_BUDGET_S, "a reasoned appeal is submittable")
    app.moderation.submit_appeal()
    _poll(
        lambda: app.moderation.has_appeal_status()
        and app.moderation.appeal_status_text() == APPEAL_RECORDED,
        _SEND_BUDGET_S,
        "the appeal is recorded",
        diagnose=lambda: app.moderation.appeal_status_text()
        if app.moderation.has_appeal_status()
        else (app.error_text() if app.has_error() else "no status"),
    )

    # The admin: Unblock, from the blocked list.
    login_as_nest_admin(app, nest_instance, nest_instance["url"])
    app.admin.navigate_nest()
    _poll(
        lambda: app.admin.blocked_index(post_id) >= 0,
        _PAGE_BUDGET_S,
        "the blocked item is listed",
        diagnose=app.admin.blocked_rows,
    )
    app.admin.unblock(app.admin.blocked_index(post_id))
    _poll(
        lambda: app.driver.count("admin-nest-report-block-confirm-button") > 0,
        _PAGE_BUDGET_S,
        "arming the unblock paints the confirm",
    )
    summary = app.admin.block_confirm_summary()
    assert summary.startswith("Unblock") and post_id in summary, summary
    app.admin.block_confirm()
    _poll(
        lambda: app.admin.blocked_index(post_id) < 0,
        _SEND_BUDGET_S,
        "an unblocked item leaves the blocked list",
        diagnose=lambda: (app.admin.blocked_rows(), app.error_text() if app.has_error() else ""),
    )

    # The same post serves again — the same id, so the same bytes.
    _login(app, nest_instance, viewer)
    _general_feed(app)
    assert app.feed.wait_for_post_text(target_text, timeout_s=_FEED_BUDGET_S), (
        "an unblocked post serves again"
    )
    assert app.feed.post_state_by_id(post_id) is not None, "the same post id re-serves"
    _poll(
        lambda: target_text in (_quote_embed_text(app, post_id) or "")
        and NOTICE not in (_quote_embed_text(app, post_id) or ""),
        _EMBED_BUDGET_S,
        "the viewer's quote paints the body again",
        diagnose=lambda: _quote_embed_text(app, post_id),
    )
