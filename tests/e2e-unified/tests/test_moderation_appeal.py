"""tier_3 e2e: the author APPEALS a legal takedown through the app UI.

``moderation.md`` § Legal takedown — every takedown owes a **transparency
triple**: a visible tombstone, ``fauna.moderation.appeal`` by ``content_id``,
and a permanent audit row. Before this module the middle leg existed only on
the wire: the RPC was live, a real ``TakenDown`` row existed to appeal against,
and **no app rendered an appeal action anywhere** (§ Implementation status
today's open-gap note). ``moderation-queue`` outcome 3, *"Appealing a
decision"*, was one of the four outcomes no column witnessed — because the
product did not do it.

The journey, all through the UI (e2e-conventions point 8 — no API call stands
in for the user):

admin console → take a post down → the author's moderation queue gains the
``TakenDown`` row **and its appeal handle** → open the appeal → the submit
control refuses a reasonless appeal (the shared fold's guard, rendered) → type
a reason → submit → the outcome line reads *recorded for review* → the queue row
**stays** (appeals are additive history, § Persistence) and is still appealable.

Two arms deliberately live elsewhere, where they are cheap and exact:

* **The nest-side gate** — an appeal against content no enforcement ever
  touched is refused (``fauna.moderation.not_found``), and an overturn does not
  revoke the handle: ``tests/api/test_moderation_appeal.py`` (convention 5 — an
  appeal failure could be UI-side or nest-side, so the nest half is asserted
  directly over the wire).
* **Which rows offer an appeal at all** — a nest-issued row does, a client-side
  local detection does not:
  ``apps/fauna-tui/src/moderation.rs::only_a_server_row_paints_the_appeal_button``
  over the shared ``fauna_client_moderation::row_is_appealable`` rule. Painting
  it in a journey would need the two-real-engine local-detection fixture for an
  assertion about a *button's absence*.

Latency-independent (convention 14): positive waits are named generous budgets
+ deadline polls; nothing here sleeps a fixed interval and then asserts.

App arm: **tui** (the lead app — ``testing.md`` § Default app and nest mode's
rust-first ordering). The other six join their trickle-down by extending
``_BUILT_APPS``, exactly as the takedown journey's list grew.
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

_BUILT_APPS = ("tui", "web", "linux", "android")

# The console's + the queue's own wording (i18n, en.yaml) — the shared verdicts
# every app renders (`fauna_client_moderation::{takedown,appeal}`).
WORKING = "Submitting…"
DONE_TAKEDOWN = "Taken down. A tombstone is served in its place and the author can appeal."
APPEAL_RECORDED = "Appeal recorded. An administrator will review it."
APPEAL_BLOCKED_NO_REASON = "Enter a reason before submitting the appeal."
# A takedown row's three facts: the `illegal` enforcement descriptor, a
# definitive confidence (1.0), and the TakenDown action's label.
TAKEDOWN_CATEGORY = "Illegal"
TAKEDOWN_CONFIDENCE = "100%"
TAKEDOWN_ACTION = "Removed under legal obligation"

REFERENCE = "Court order 42/2026"
REASON = "I hold the licence for this recording."

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0     # admin-nest page render after the nav patch
_VERDICT_BUDGET_S = 60.0  # one WS-RPC round-trip + the status repaint
_QUEUE_BUDGET_S = 30.0    # the moderation queue's actions read after nav


def _poll(check, budget_s, tag):
    """Deadline-poll ``check`` until truthy; the failure names the budget."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    raise AssertionError(f"{tag}: not reached within {budget_s}s")


def _require_built(app):
    if app_name(app.driver) not in _BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="appeal-button",
            detail="the appeal affordance (moderation.md § Legal takedown — "
            "the transparency triple's second leg) is built on tui first",
            tracked="",
        )


def _take_down_own_post(app, *, expect_appeal: bool = True):
    """Post, take it down through the admin console, and wait for the author's
    queue to gain exactly one ``TakenDown`` row. Returns the queue's row count
    before the takedown (the session's admin outlives each test, so an earlier
    test's decision may already be on record). ``expect_appeal`` also asserts
    the row's appeal handle — the appeal journeys' premise, built on tui only
    (``_BUILT_APPS``); a witness of the queue row alone passes ``False``."""
    token = uuid.uuid4().hex[:10]
    target_text = f"appeal target {token}"

    # The content under legal obligation — the admin's own post (an admin is a
    # user with an extra role; the author-side observables below are theirs).
    app.feed.navigate()
    app.feed.create_post(target_text)
    row = app.feed.wait_for_post_state_by_text(target_text)
    assert row is not None, "the target post must land in feed state"
    post_id = row["post_id"]

    # Author-side baseline, so every row asserted below is attributable to
    # this takedown alone: one more row and one more appeal handle.
    app.moderation.navigate()
    assert not app.has_error()
    baseline_rows = app.moderation.correction_count()
    baseline_appeals = app.moderation.appeal_count()

    # --- The takedown, through the console (the journey's premise, already
    # witnessed in full by test_admin_legal_takedown.py — driven here only far
    # enough to produce the decision this test appeals).
    app.admin.navigate_nest()
    _poll(
        lambda: app.driver.count("admin-nest-takedown-content-id-input") > 0,
        _PAGE_BUDGET_S,
        "admin-nest renders the takedown console",
    )
    app.admin.takedown_fill(post_id, REFERENCE)
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
    verdict = app.admin.takedown_status_text()
    assert verdict == DONE_TAKEDOWN, (
        f"the takedown must succeed for this journey to have a decision to "
        f"appeal, got: {verdict!r} "
        f"(error-message: {app.error_text() if app.has_error() else 'none'})"
    )

    # --- The transparency triple's SECOND leg: the queue row arrives carrying
    # its appeal handle. Before this row that handle existed only on the wire.
    app.moderation.navigate()
    _poll(
        lambda: app.moderation.correction_count() > baseline_rows,
        _QUEUE_BUDGET_S,
        "the author's moderation queue gains the TakenDown row",
    )
    assert app.moderation.correction_count() == baseline_rows + 1, (
        "one takedown must add exactly one queue row"
    )
    if expect_appeal:
        assert app.moderation.appeal_count() == baseline_appeals + 1, (
            "the nest-issued enforcement row must offer exactly one appeal handle"
        )
    return baseline_rows


@pytest.mark.feature("moderation-queue")
def test_the_author_appeals_a_takedown_through_the_app(admin_app):
    """takedown → the queue's appeal handle → guard → reason → recorded → the
    row survives, still appealable."""
    app = admin_app
    _require_built(app)
    rows = _take_down_own_post(app) + 1

    # --- The guard, rendered: an appeal with no reason is not merely refused
    # by the nest — the form never offers the dispatch, and says why.
    app.moderation.open_appeal(0)
    _poll(
        app.moderation.appeal_form_open,
        _PAGE_BUDGET_S,
        "the appeal form opens on the row",
    )
    assert not app.moderation.appeal_submit_enabled(), (
        "a reasonless appeal must not be submittable"
    )

    app.moderation.fill_appeal_reason(REASON)
    _poll(
        app.moderation.appeal_submit_enabled,
        _PAGE_BUDGET_S,
        "a reasoned appeal becomes submittable",
    )

    # --- The dispatch, and the outcome line. `appeal-status`, never
    # `error-message`: a recorded appeal is the common case, and a refusal is
    # form feedback that must not blank the queue the user is reading.
    app.moderation.submit_appeal()
    _poll(
        lambda: app.moderation.has_appeal_status()
        and app.moderation.appeal_status_text() != WORKING,
        _VERDICT_BUDGET_S,
        "the appeal reports its outcome",
    )
    outcome = app.moderation.appeal_status_text()
    assert outcome == APPEAL_RECORDED, (
        f"the appeal must end on the recorded verdict, got: {outcome!r} "
        f"(error-message: {app.error_text() if app.has_error() else 'none'})"
    )
    assert not app.has_error(), (
        "a recorded appeal is not a page error — the queue stays readable"
    )
    assert not app.moderation.appeal_form_open(), (
        "a recorded appeal closes its form (the decision is with an admin now)"
    )

    # --- Additive history (§ Persistence): the appeal does not erase the
    # decision. The row stays, and stays appealable — the user may appeal
    # again, and the audit trail carries every attempt.
    assert app.moderation.correction_count() == rows, (
        "an appeal is additive history — it must not remove the queue row"
    )
    assert app.moderation.appeal_count() >= 1, (
        "the decision is still on record, so its handle is still offered"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("moderation-queue")
def test_a_queue_row_shows_its_category_confidence_and_action(admin_app):
    """A nest-issued row shows what it was flagged as, how confident the flag
    is, and the action taken (moderation.md § Layout & flow — badge +
    confidence + the optional enforcement-action label). No appeal affordance
    is read, so the appeal gate (`_require_built`) does not apply here."""
    app = admin_app
    _take_down_own_post(app, expect_appeal=False)

    category = app.moderation.row_category(0)
    confidence = app.moderation.row_confidence(0)
    action = app.moderation.row_action(0)
    facts = f"category={category!r} confidence={confidence!r} action={action!r}"
    assert TAKEDOWN_CATEGORY in category, (
        f"the row's badge must name what it was flagged as ({facts})"
    )
    assert confidence == TAKEDOWN_CONFIDENCE, (
        f"the row must show the flag's confidence as a whole percent ({facts})"
    )
    assert action == TAKEDOWN_ACTION, (
        f"the row must show the action taken on it ({facts})"
    )
