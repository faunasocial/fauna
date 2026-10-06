"""tier_3 e2e: the ADMIN initiates — and overturns — a legal takedown through the app UI.

``moderation.md`` § Legal takedown (the legal-compulsion carve-out) + its
invocation surface (ruled 2026-08-16 — the close): before this module,
``fauna.moderation.legal_takedown`` had no caller on any app — an admin could
reach a compulsory, legally bounded action only over raw WS-RPC, against the
one-configuration-surface invariant (`principles.md` § One configuration
surface). This journey drives the transparency triple end to end through the UI
that invariant demands:

nav ``admin-nest`` → fill (content id + citation) → the arm button refuses a
citation-less takedown (the structural guard, rendered) → arm → the confirm
names the content AND the citation before anything dispatches → confirm →
verdict → the post leaves the feed → the author's own moderation queue gains
the ``TakenDown`` row → overturn through the same console (restore mode, where
an empty reference is legal — the guard's deliberate asymmetry) → the post
serves again.

Mutations are UI-only (e2e-conventions point 8): the takedown, the restore, and
every feed re-pull barrier (a fresh compose) go through the app. The only
state-protocol read is the post-id lookup — the datum the console needs typed
into it, not a mutation shortcut.

Queue asserts are DELTAS off a baseline read, never an assert-empty: ``admin_app``
is THE nest admin — one identity on every app — over a session-scoped nest, so a
second app leg in the same invocation starts with the first leg's own
``TakenDown`` row still standing (this journey's last assertion is that it
survives a restore). See the baseline read's own comment.

Latency-independent (convention 14): positive waits are named generous budgets +
deadline polls; the feed-exclusion asserts ride a **compose barrier** — a
compose ends in ``FeedManager::refresh_current_feed``, so once the marker post
renders, the re-pull the exclusion needs has provably answered (the
lesson: compose carries the barrier; a like does not).

App arm: **tui** (the lead app — ``testing.md`` § Default app and nest mode's
rust-first ordering). The other six join their trickle-down by extending
``_BUILT_APPS``, exactly as the seed-rotation journey's list grew. linux,
android, web and windows joined there; macOS and iOS joined together, since
the console is ONE ``FaunaKit`` view over the same
``fauna_client_moderation::takedown`` fold the other five render.
"""

import time
import uuid

import pytest

from helpers.app_surface import app_name, skip_unbuilt

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
]

_BUILT_APPS = ("tui", "linux", "android", "web", "windows", "macos", "ios")

# The console's own wording (i18n `admin.nest_page.takedown_*`, en.yaml) — the
# shared verdict every app renders (`fauna_client_moderation::takedown`).
WORKING = "Submitting…"
DONE_TAKEDOWN = "Taken down. A tombstone is served in its place and the author can appeal."
DONE_RESTORE = "Restored. The content is served again; the takedown stays on record."

REFERENCE = "Court order 42/2026"

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # admin-nest page render after the nav patch
_VERDICT_BUDGET_S = 60.0  # one WS-RPC round-trip + the status repaint
_FEED_BUDGET_S = 30.0     # the compose-barrier re-pull reflecting the flag
_QUEUE_BUDGET_S = 30.0    # the moderation queue's actions read after nav


def _poll(check, budget_s, tag):
    """Deadline-poll ``check`` until truthy; the failure names the budget."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    raise AssertionError(f"{tag}: not reached within {budget_s}s")


@pytest.mark.feature("admin-nest", "moderation-queue")
def test_the_admin_takes_down_and_restores_a_post_through_the_app(admin_app):
    """fill → guard → arm → named confirm → verdict → feed exclusion → queue
    row → restore → the post serves again."""
    app = admin_app
    if app_name(app.driver) not in _BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="admin-nest-takedown-section",
            detail="the legal-takedown console (moderation.md § Legal takedown, "
            "invocation surface) is built on tui first",
            tracked="",
        )

    token = uuid.uuid4().hex[:10]
    target_text = f"takedown target {token}"

    # The content under legal obligation — the admin's own post (an admin is a
    # user with an extra role; the author-side observables below are theirs).
    app.feed.navigate()
    app.feed.create_post(target_text)
    row = app.feed.wait_for_post_state_by_text(target_text)
    assert row is not None, "the target post must land in feed state"
    post_id = row["post_id"]

    # Author-side baseline: the queue's count BEFORE this takedown, so the row
    # asserted later is attributable to this takedown alone.
    #
    # Deliberately a BASELINE, not an assert-empty. ``admin_app`` is THE nest
    # admin — one identity, injected on every app — on a session-scoped
    # ``nest_instance``, so a second app leg in the same invocation inherits
    # the first leg's own ``TakenDown`` row, which this very journey proves
    # survives its restore as additive history. An assert-empty therefore
    # passes only for whichever app pytest happens to run FIRST, and that is a
    # property of the invocation, not of the app: it went unnoticed while every
    # column ran one app per machine, and fails the moment two do
    # (``--app ios,macos``, the apple idiom, 2026-09-21). The delta below
    # asserts exactly what the outcome claims — one takedown, one new row.
    app.moderation.navigate()
    assert not app.has_error()
    baseline_rows = app.moderation.correction_count()

    # The console.
    app.admin.navigate_nest()
    _poll(
        lambda: app.driver.count("admin-nest-takedown-content-id-input") > 0,
        _PAGE_BUDGET_S,
        "admin-nest renders the takedown console",
    )

    # The structural guard, rendered: a takedown with no legal reference is not
    # merely refused by the nest — the console never arms it (the shared form
    # view's `can_submit`; the reference is what makes this compulsion, not
    # policy).
    app.admin.takedown_fill(post_id)
    assert not app.driver.is_enabled("admin-nest-takedown-button"), (
        "a citation-less takedown must not be armable"
    )

    app.admin.takedown_fill(post_id, REFERENCE)
    _poll(
        lambda: app.driver.is_enabled("admin-nest-takedown-button"),
        _PAGE_BUDGET_S,
        "a well-formed takedown becomes armable",
    )
    app.admin.takedown_arm()

    # The confirm is the decision surface: it must name the content and the
    # citation BEFORE anything dispatches — a compulsory act is never confirmed
    # blind.
    assert app.driver.count("admin-nest-takedown-confirm-button") > 0, (
        "arming must paint the confirm surface"
    )
    summary = app.admin.takedown_confirm_summary()
    assert post_id[:12] in summary, (
        f"the confirm must name the content it will withhold, got: {summary!r}"
    )
    assert REFERENCE in summary, (
        f"the confirm must name the citation it will record, got: {summary!r}"
    )

    app.admin.takedown_confirm()
    # Disarm-before-dispatch: nothing left to double-click once the click
    # replies (the seed-rotation shape).
    assert app.driver.count("admin-nest-takedown-confirm-button") == 0, (
        "the first confirm click must disarm the surface"
    )
    _poll(
        lambda: app.driver.count("admin-nest-takedown-status") > 0
        and app.admin.takedown_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        "the takedown reports its verdict",
    )
    verdict = app.admin.takedown_status_text()
    assert verdict == DONE_TAKEDOWN, (
        f"the takedown must end on the success verdict, got: {verdict!r} "
        f"(error-message: {app.error_text() if app.has_error() else 'none'})"
    )

    # Serve-side: feeds exclude the post. The marker compose is the re-pull
    # barrier — once it renders, the exclusion has had its read.
    app.feed.navigate()
    app.feed.create_post(f"takedown marker one {token}")
    assert app.feed.wait_for_post_text_absent(target_text, _FEED_BUDGET_S), (
        "a taken-down post must leave the feed on the next re-pull"
    )

    # Author-side: the transparency triple's queue row (`ObligationAction::
    # TakenDown`, surfaced via fauna.moderation.actions) — the appealable half.
    app.moderation.navigate()
    _poll(
        lambda: app.moderation.correction_count() > baseline_rows,
        _QUEUE_BUDGET_S,
        "the author's moderation queue gains the TakenDown row",
    )
    assert app.moderation.correction_count() == baseline_rows + 1, (
        "exactly one NEW obligation row should exist for one takedown"
    )

    # The overturn, through the same console. Restore mode: an empty reference
    # is LEGAL here (the overturn note is optional) — the guard's asymmetry is
    # itself the assert.
    app.admin.navigate_nest()
    _poll(
        lambda: app.driver.count("admin-nest-takedown-content-id-input") > 0,
        _PAGE_BUDGET_S,
        "admin-nest renders the takedown console (restore leg)",
    )
    app.admin.takedown_fill(post_id, restore=True)
    _poll(
        lambda: app.driver.is_enabled("admin-nest-takedown-button"),
        _PAGE_BUDGET_S,
        "a restore with no note must still be armable",
    )
    app.admin.takedown_arm()
    summary = app.admin.takedown_confirm_summary()
    assert post_id[:12] in summary, (
        f"the restore confirm must name the content it re-serves, got: {summary!r}"
    )
    app.admin.takedown_confirm()
    _poll(
        lambda: app.driver.count("admin-nest-takedown-status") > 0
        and app.admin.takedown_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        "the restore reports its verdict",
    )
    verdict = app.admin.takedown_status_text()
    assert verdict == DONE_RESTORE, (
        f"the restore must end on the restored verdict, got: {verdict!r} "
        f"(error-message: {app.error_text() if app.has_error() else 'none'})"
    )

    # The content serves again — and the takedown row stays, as additive
    # history (tombstone-not-delete; restore is always possible, forgetting is
    # not).
    app.feed.navigate()
    app.feed.create_post(f"takedown marker two {token}")
    assert app.feed.wait_for_post_text(target_text, _FEED_BUDGET_S), (
        "a restored post must serve again on the next re-pull"
    )
    app.moderation.navigate()
    _poll(
        lambda: app.moderation.correction_count() > baseline_rows,
        _QUEUE_BUDGET_S,
        "the obligation row survives the restore (additive history)",
    )
