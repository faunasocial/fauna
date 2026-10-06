"""`calendar-visibility` — the per-calendar show/hide DISPLAY filter.

Owner: `docs/goal/ui/events.md` § Where logic lives → *Which calendars display*
(ratified 2026-08-02 as the id's ONE meaning, all 7 apps; the predicate is
shared Rust — `fauna_client_caldav::calendar_is_displayed`).

**Why this file exists at all.** Until 2026-08-02 one ui.yaml id was implemented
as three incompatible concepts — linux's real display filter, a create-form
private/public picker on web/android/macos/ios whose value died before the wire,
and a `Visibility="Collapsed"` list-collapse button on windows — and *no test
ever caught it*, because the only action touching the id was a bare
`click("calendar-visibility")` with no scoping and no assertion. A click-only
check passes against all three. This asserts the **concept**: unchecking a
calendar removes exactly that calendar's events from the displayed union, and
re-checking restores them.

**Why the relaunch in the middle is load-bearing.** The filter composes onto the
**no-selection arm only** — a live `calendar-item` selection wins outright and
the boxes do not apply (events.md § Where logic lives). Seeding an event
necessarily leaves a selection behind (creating one targets, and then selects,
its calendar), and no app ships a "deselect"/"show all" affordance — the union is
the *fresh-page* state. So the union arm is reached the only way the product
offers: a fresh app instance. That is the same cold relaunch the `app` fixture
performs at every module boundary (testing.md § point 10), taken explicitly
mid-test.
"""

import uuid
from datetime import datetime, timedelta

import pytest

from conftest import _login_app_as
from helpers.app_surface import declared_absence

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.reclaim_cycle]

# Generous, latency-independent budget (convention 14): the ceiling sits far
# above any non-pathological re-render, and a green run pays one poll.
FILTER_BUDGET_S = 30


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _future_dt(days: int, hour: int) -> str:
    """Same shape `test_events.py` uses — relative to today so the test never
    rots into the past, and the bare `HH:MM` the shared
    `normalize_event_datetime_input` rule expects."""
    d = datetime.now() + timedelta(days=days)
    return d.replace(hour=hour, minute=0, second=0, microsecond=0).strftime(
        "%Y-%m-%dT%H:%M"
    )


@pytest.mark.feature("calendar-and-events")
def test_calendar_visibility_filters_the_displayed_union(
    logged_in_app, calendar_backend, request, nest_instance, test_user
):
    """Unchecking a calendar drops **its** events from the union, and only its own.

    The whole contract in one journey: two calendars, one event each, both listed
    on the union arm; uncheck the first → its event leaves and the other stays;
    re-check → it comes back.
    """
    # The union arm is reachable only through a cold relaunch (see the module
    # docstring), so an app that cannot relaunch structurally cannot run this —
    # the same declared absence `test_module_relaunch.py` pins for the fixture's
    # own module-boundary relaunch, not unbuilt debt.
    if not logged_in_app.driver.supports_cold_relaunch():
        declared_absence(
            logged_in_app.driver,
            capability="cold app relaunch (needed to reach the no-selection "
            "union arm, the only arm calendar-visibility composes onto)",
            doc="testing.md § point 10 (per-module cold relaunch); pinned by "
            "tests/test_module_relaunch.py",
        )

    # `calendar_backend` (conftest) has minted the actor's MSEK — without it the
    # Events page degrades to an empty state and every create silently no-ops.
    ev = logged_in_app.events
    cal_a, cal_b = _unique("visA"), _unique("visB")
    event_a, event_b = _unique("EventA"), _unique("EventB")

    # ── 1. Seed: one event in each of two fresh calendars. Creating targets (and
    # then selects) its calendar, so the page is on the SELECTION arm after this.
    ev.navigate()
    ev.create_calendar(cal_a)
    ev.select_calendar(cal_a)
    ev.create_event(summary=event_a, start=_future_dt(7, 10), end=_future_dt(7, 11))

    ev.create_calendar(cal_b)
    ev.select_calendar(cal_b)
    ev.create_event(summary=event_b, start=_future_dt(8, 14), end=_future_dt(8, 15))

    # ── 2. Reach the no-selection UNION arm the only way the product offers: a
    # fresh instance. `selected_calendar` is in-memory page state on every app,
    # so a cold start has no selection and the union is what paints.
    assert logged_in_app.driver.recover(), (
        "client relaunch (driver.recover()) failed — cannot reach the no-selection "
        "union arm, which is the only arm calendar-visibility composes onto"
    )
    _login_app_as(logged_in_app, request, nest_instance, test_user)
    # The fresh data dir holds no cached mail key, so the app must re-fetch the
    # mail config (incl. the wrapped MSEK) before it can open the sealed CalDAV
    # store again. Idempotent — mail is already enabled server-side; this is the
    # same post-restart confirm `test_mail_sent_copy_restart.py` makes.
    logged_in_app.mail_settings.navigate()
    logged_in_app.mail_settings.ensure_mail_enabled()
    ev.navigate()

    # Identity, not a count: the session actor accumulates calendars across the
    # run, so the union legitimately lists other tests' events too.
    summaries = ev.event_summaries()
    assert event_a in summaries and event_b in summaries, (
        f"both events must be listed on the no-selection union before filtering — "
        f"got {summaries!r}"
    )

    # ── 3. THE ASSERTION the bare click never made: unchecking cal_a removes
    # exactly cal_a's event. `hide_calendar` scopes the click to that calendar's
    # own row (the id is `indexed: true`) and waits for the union to narrow.
    ev.hide_calendar(cal_a, event_a, timeout=FILTER_BUDGET_S)
    summaries = ev.event_summaries()
    assert event_a not in summaries, (
        f"unchecking {cal_a!r} must drop its event from the displayed union — "
        f"got {summaries!r}"
    )
    assert event_b in summaries, (
        f"unchecking {cal_a!r} must NOT touch another calendar's events — a filter "
        f"that hides everything (or nothing) passes a click-only check and fails "
        f"here; got {summaries!r}"
    )

    # ── 4. And it is a *filter*, not a delete: re-checking restores the row.
    ev.show_calendar(cal_a, event_a, timeout=FILTER_BUDGET_S)
    summaries = ev.event_summaries()
    assert event_a in summaries and event_b in summaries, (
        f"re-checking {cal_a!r} must restore its events to the union — got "
        f"{summaries!r}"
    )


@pytest.mark.feature("calendar-and-events")
def test_no_calendar_picked_shows_every_calendar_and_picking_one_narrows(
    logged_in_app, calendar_backend, request, nest_instance, test_user
):
    """With no calendar picked the page shows every calendar's events together,
    and picking one narrows it to that calendar's alone (``docs/goal/ui/events.md``
    § Layout & flow — "the no-selection union of all owned calendars stays
    visible"; § Where logic lives → *Which calendars display*).

    Both arms, asserted on the page by name: the union lists both calendars'
    events; picking the first shows its event and NOT the second's; picking the
    second flips that. ``select_calendar``'s own barrier is no witness here — it
    only waits when the picked calendar is still empty, and these are not.

    The union arm is reached the way the product offers — a fresh app instance,
    which starts with nothing picked (see the module docstring).
    """
    if not logged_in_app.driver.supports_cold_relaunch():
        declared_absence(
            logged_in_app.driver,
            capability="cold app relaunch (the no-selection union is the fresh-page "
            "state, reachable only through a new app instance)",
            doc="testing.md § point 10 (per-module cold relaunch); pinned by "
            "tests/test_module_relaunch.py",
        )

    ev = logged_in_app.events
    cal_a, cal_b = _unique("unionA"), _unique("unionB")
    event_a, event_b = _unique("UnionEventA"), _unique("UnionEventB")

    ev.navigate()
    ev.create_calendar(cal_a)
    ev.select_calendar(cal_a)
    ev.create_event(summary=event_a, start=_future_dt(9, 10), end=_future_dt(9, 11))
    ev.create_calendar(cal_b)
    ev.select_calendar(cal_b)
    ev.create_event(summary=event_b, start=_future_dt(10, 14), end=_future_dt(10, 15))

    assert logged_in_app.driver.recover(), (
        "client relaunch (driver.recover()) failed — cannot reach the no-selection "
        "union, which only a fresh page shows"
    )
    _login_app_as(logged_in_app, request, nest_instance, test_user)
    logged_in_app.mail_settings.navigate()
    logged_in_app.mail_settings.ensure_mail_enabled()
    ev.navigate()

    # ── Nothing picked: every calendar's events, together.
    shown = ev.wait_for_listed(present=[event_a, event_b], timeout=FILTER_BUDGET_S)
    assert event_a in shown and event_b in shown, (
        f"with no calendar picked, both {cal_a!r}'s and {cal_b!r}'s events should "
        f"be listed together; got {shown!r}"
    )

    # ── Picking one narrows the page to it — and only it.
    for picked, kept, dropped in ((cal_a, event_a, event_b), (cal_b, event_b, event_a)):
        ev.select_calendar(picked)
        shown = ev.wait_for_listed(present=[kept], absent=[dropped], timeout=FILTER_BUDGET_S)
        assert kept in shown and dropped not in shown, (
            f"picking {picked!r} should show its event {kept!r} and not the other "
            f"calendar's {dropped!r}; got {shown!r}"
        )
    assert not logged_in_app.has_error(), (
        f"unexpected events error: {logged_in_app.error_text()!r}"
    )
