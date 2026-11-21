"""Events on a fresh account: the account's key is minted at its first connect,
so ``Personal`` is listed with no step taken, and an event is made and seen.

Goal: ``docs/goal/behavior/mail-credentials.md`` § MSEK lifecycle → *Every
account holds the key* (rulings 3 and 7) and ``docs/goal/ui/events.md``
§ Errors & edge cases → *No calendar to hold an event*, the page's instance of
``docs/goal/ui/README.md`` § Offer only what is possible, rule 1: the
precondition is one the product makes true by itself.

**Why this journey exists.** Every other Events test takes the autouse
``calendar_backend`` fixture (``test_events.py``), which mints the actor's
calendar key before the page is opened, so no test ever stood on the state a
freshly claimed account is really in. This journey takes NO calendar-seeding
fixture (convention 17: fixtures do not seed past the frontier under test). It
signs in a dedicated actor that is guaranteed never to have enabled mail
(``dedicated_no_mail_app``), and every mutation goes through the app's own UI
(convention 8).

**History.** Until 2026-11-18 a fresh account on the e2e nest held no key at
all, and this journey proved the compose stayed shut until the page's named
step (a first mail credential) was taken. The key mint at first connect
retired that state: the step is no longer part of the journey, and the page
must never offer it to an account that only waited for its key.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timedelta

import pytest
from helpers.nav_tab_click import click_nav_tab
from helpers.use_case_walk import assert_use_case_walked, setup_gestures

pytestmark = [pytest.mark.tier_3]


@pytest.mark.use_case("add-a-calendar-event")
@pytest.mark.feature("calendar-and-events")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
def test_a_fresh_account_holds_its_calendar_with_no_step_and_creates_an_event(
    dedicated_no_mail_app,
):
    app = dedicated_no_mail_app
    driver = app.driver
    ev = app.events

    # 1. Fresh account, nothing seeded past sign-in and no credential taken:
    #    the key minted at the first connect makes the lazy `Personal`
    #    calendar listed (rulings 3 and 7 — while the mint is in flight the
    #    page reads loading, and its store-change re-run lists the calendar).
    #    The tab is the use case's first step; the agenda view `navigate()`
    #    picks and the calendar check below are this test's own reading of its
    #    world, declared as setup (main-use-cases.md § The journeys).
    click_nav_tab(driver, "events-tab", canary="calendar-view-agenda")
    with setup_gestures(driver):
        ev.navigate()
    ev._wait_for_calendar("Personal")
    painted = [
        str(rec.get("id") or "") for rec in (driver.registry_snapshot() or [])
    ]
    assert "calendar-list-empty-state-step" not in painted, (
        "the page offered a step on an account that only waited for its key: "
        f"{painted!r}, {driver.diagnose('calendar-list-empty-state')}"
    )

    # 2. An event made through the compose is seen.
    with setup_gestures(driver):
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
    assert_use_case_walked(app.driver, "add-a-calendar-event")
