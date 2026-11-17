"""Events on a fresh account: no calendar can hold an event, so the compose never
opens; the step the page names is taken; then ``Personal`` exists and an event
is made and seen.

Goal: ``docs/goal/ui/events.md`` § Errors & edge cases → *No calendar to hold an
event* (rulings 1 and 3), the page's instance of ``docs/goal/ui/README.md``
§ Offer only what is possible.

**Why this journey exists.** Every other Events test takes the autouse
``calendar_backend`` fixture (``test_events.py``), which mints the actor's
calendar key before the page is opened, so no test ever stood on the state a
freshly claimed account is really in. On the e2e nest, which is claimed on a
localhost handle, a fresh account holds no key at all (``onboarding.md`` § 3b).
That is the *no key* answer of ruling 2. This journey takes NO calendar-seeding
fixture (convention 17: fixtures do not seed past the frontier under test). It
signs in a dedicated actor that is guaranteed never to have enabled mail
(``dedicated_no_mail_app``), and every mutation goes through the app's own UI
(convention 8).

**Red-verified.** Before the tui lead fix every door opened the compose and
the submit sent an empty calendar id; step 1 raised
``ComposeOpenedWithNoCalendar`` then (it was carried as a strict expected red
until the doors were gated on ``fauna_client_caldav::event_create_possibility``).

**The reason and its step are read off the page.** Ruling 3 paints the blocked
reason in the calendar list's empty state (``calendar-list-empty-state``) with
a one-press jump to the step's place (``calendar-list-empty-state-step``).
Step 1 reads the reason there; step 2 takes the step through that jump.
"""

from __future__ import annotations

import uuid
from datetime import date, datetime, timedelta

import pytest

from i18n.strings import S

pytestmark = [pytest.mark.tier_3]


class ComposeOpenedWithNoCalendar(AssertionError):
    """Step 1's red: a door to the event compose opened on an account whose
    calendar read cannot hold an event. This is the one failure the expected-red
    mark accepts."""


def _compose_is_open(driver) -> bool:
    """Whether any part of the event compose is registered. ``event-summary``
    and ``event-dtstart`` are the fields every app's ``event-form`` carries;
    either one being present means the compose opened."""
    return driver.is_visible("event-summary") or driver.is_visible("event-dtstart")


def _registered_texts(driver) -> list[str]:
    snapshot = driver.registry_snapshot()
    if snapshot is None:
        return []
    return [str(rec.get("text") or "") for rec in snapshot]


def _assert_no_door_opens(app) -> None:
    """Step 1: every door to the compose is shut, and the page does not claim a
    resting empty list.

    Latency-independent (convention 14). Each door is actuated, then the app's
    ``barrier()`` (tui drains its UI message channel) makes sure the gesture's
    own work has run before its absence is asserted. While the calendar read is
    still unresolved, the predicate answers *blocked, loading*, so the compose
    must not open then either. The assertion therefore needs no anchor on the
    read having finished.
    """
    driver = app.driver
    ev = app.events

    # The New Event opener.
    driver.wait_for("new-event-btn", timeout=10)
    if driver.is_enabled("new-event-btn"):
        driver.click("new-event-btn")
    driver.barrier()
    if _compose_is_open(driver):
        raise ComposeOpenedWithNoCalendar(
            "new-event-btn opened the event compose on an account with no calendar "
            "that can hold an event (events.md, No calendar to hold an event, "
            f"ruling 3): {driver.diagnose('new-event-btn', attrs=('enabled',))}, "
            f"{driver.diagnose('event-summary')}, {driver.diagnose('error-message')}"
        )

    # The month-grid day double-click: the positional gesture is not live.
    ev.switch_view("month")
    today = date.today().isoformat()
    assert ev.has_day_cell(today), (
        f"the month grid should paint today's cell: "
        f"{driver.diagnose(f'events-day-cell-{today}')}"
    )
    driver.double_click(f"events-day-cell-{today}")
    driver.barrier()
    if _compose_is_open(driver):
        raise ComposeOpenedWithNoCalendar(
            f"double-clicking events-day-cell-{today} opened the event compose on an "
            "account with no calendar that can hold an event (events.md, No calendar "
            f"to hold an event, ruling 3): {driver.diagnose('event-dtstart')}, "
            f"{driver.diagnose('error-message')}"
        )
    ev.switch_view("agenda")

    # The page tells the truth about why. The opener must answer not-enabled,
    # which is the predicate's blocked answer reaching the door (tui's
    # possibility seam desensitizes it), and the page must not paint the bare
    # resting-empty string that ruling 3 retires.
    assert not driver.is_enabled("new-event-btn"), (
        "new-event-btn should be desensitized while no calendar can hold an event: "
        f"{driver.diagnose('new-event-btn', attrs=('enabled',))}"
    )
    painted = _registered_texts(driver)
    assert S.events.no_calendars not in painted, (
        f"the calendar list says {S.events.no_calendars!r} on an account with no "
        "calendar key; ruling 3 requires a reason that names the unblocking step "
        f"and its place. Painted: {painted!r}"
    )
    # The reason names the step and its place. The e2e nest serves mail, so
    # the no-key reason is Add credential's. The element appears once the read
    # has answered (while unresolved the page paints its loading line), so the
    # wait is on that state, not on a delay.
    driver.wait_for("calendar-list-empty-state", timeout=15)
    reason = driver.get_text("calendar-list-empty-state")
    assert reason == S.events.create_blocked.add_credential, (
        f"the empty state should name the unblocking step: {reason!r}, "
        f"{driver.diagnose('calendar-list-empty-state')}"
    )
    assert driver.is_enabled("calendar-list-empty-state-step"), (
        "the one-press jump to the step's place should be offered: "
        f"{driver.diagnose('calendar-list-empty-state-step', attrs=('enabled',))}"
    )


@pytest.mark.feature("calendar-and-events")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
def test_a_fresh_account_cannot_compose_until_the_named_step_then_creates_an_event(
    dedicated_no_mail_app,
):
    app = dedicated_no_mail_app
    driver = app.driver
    ev = app.events

    # 1. Fresh account, nothing seeded past sign-in: no door opens the compose.
    ev.navigate()
    _assert_no_door_opens(app)

    # 2. Take the unblocking step through the app's own UI: the page's jump
    #    lands on Mail settings, where Add credential's first credential is the
    #    enable (mail-credentials.md § Trigger taxonomy, The calendar-only
    #    credential; ruling 3's no-key reason).
    driver.click("calendar-list-empty-state-step")
    driver.barrier()
    app.mail_settings.ensure_mail_enabled()

    # 3. Back on Events, the lazy `Personal` calendar is listed with no further
    #    act (ruling 1), and an event made through the compose is seen.
    ev.reenter()
    ev._wait_for_calendar("Personal")
    ev.select_calendar("Personal")
    summary = f"fresh-account-{uuid.uuid4().hex[:8]}"
    start = (datetime.now() + timedelta(days=1)).replace(
        hour=10, minute=0, second=0, microsecond=0
    )
    ev.create_event(
        summary,
        start.strftime("%Y-%m-%dT%H:%M"),
        (start + timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M"),
    )
    assert summary in ev.event_summaries(), (
        f"the event made on the fresh account should be listed: "
        f"{ev.event_summaries()!r}, {driver.diagnose('event-card')}, "
        f"{driver.diagnose('error-message')}"
    )
