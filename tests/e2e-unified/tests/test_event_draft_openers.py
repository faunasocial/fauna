"""tier_3: which way the new-event form is opened decides whether it brings back
the half-written event — ``docs/goal/ui/events.md`` § Persistence, *Opening the
compose resumes the draft; a day-cell gesture starts fresh*, with the rule that
makes resuming more than an open-time read in
``docs/goal/behavior/reserved-folders.md`` § Drafts Sync → *Implementation status
today*: "the resume must be NON-DESTRUCTIVE, and the rail — not the compose
surface — must hold the live draft".

``test_event_draft_persistence.py`` proves the draft survives a restart, and by
construction opens the form exactly once afterwards. That is why it cannot see
the bug the second rule was written for (measured on web 2026-08-31): an app
that hands the restored draft to the FIRST opener and keeps nothing for the next
shows an empty form the second time, while the nest still holds the draft. These
journeys never restart; they open the form again, and again, and the other way.

1. **Leaving and coming back resumes.** tui's event form has no close control of
   its own (ui.yaml's ``event-form`` carries none), so on tui closing the form IS
   leaving the page: the Events tab lands back on the calendar on its nav edge
   (``apps/fauna-tui/src/events/mod.rs::on_nav_enter``). The journey asserts the
   form really closed before it opens it again, twice. linux's form is a modal
   dialog over the page, which a user closes rather than navigates past — and a
   page change underneath leaves it standing — so on linux the leave starts with
   Escape, the dialog's own close (``apps/fauna-linux/src/views/events/event_form.rs``);
   outcome 11 reads "when you leave the page or close the form".
2. **A day cell starts fresh, and a created event leaves the next form blank.**
   The day-cell double-click means *start a new event here* (§ Layout & flow), so
   it opens an empty form at that day instead of the saved draft; and once that
   event is created, New Event — the opener that resumes — has nothing to resume.

**A dedicated account**, with its own calendar: the form's openers are reached
behind a calendar on every app (web renders New Event only when one exists), and
a fresh account holds no resting draft from an earlier test for New Event to
resume. The calendar surface needs the account's mail key first — the
``calendar_backend`` fixture's two steps, taken here for this account.

Latency-independent (convention 14): every wait polls the form's own fields or
its presence, under a ceiling a green run never pays.
"""

import time
import uuid
from datetime import date

import pytest

from actions.events import EVENT_CREATE_BUDGET_S
from helpers.app_surface import app_name
from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; the other six join through the drafts-survive cross-app lift.
    pytest.mark.tui,
]


def _poll(read, done, timeout: float = 10.0):
    """Read until ``done(value)`` or the budget runs out; returns the last value
    read, for the failure message."""
    deadline = time.monotonic() + timeout
    value = read()
    while not done(value) and time.monotonic() < deadline:
        time.sleep(0.2)
        value = read()
    return value


def _field(driver, element_id: str) -> str:
    return driver.get_text(element_id) if driver.is_visible(element_id) else ""


def _fresh_account_with_calendar(app, request, nest_instance) -> str:
    """Log ``app`` in as a dedicated fresh account, give it the mail key the
    encrypted calendar store needs (``calendar_backend``'s steps), and create a
    calendar. Returns the calendar's name."""
    from conftest import _login_app_as, _make_user

    _login_app_as(app, request, nest_instance, _make_user(nest_instance),
                  verify_live_actor=True)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    name = f"drafts-{uuid.uuid4().hex[:8]}"
    app.events.navigate()
    app.events.create_calendar(name)
    app.events.select_calendar(name)
    return name


def _leave_and_return(app) -> None:
    """Leave the Events page for another one and come back — on tui, the way the
    new-event form closes; on linux, after closing its dialog with Escape
    (module docstring, point 1)."""
    if app_name(app.driver) == "linux":
        app.driver.press_key("event-summary", "Escape")
    app.feed.navigate()
    app.events.navigate()


def _assert_form_closed(app, what: str) -> None:
    open_ = _poll(lambda: app.driver.is_visible("event-summary"), lambda v: not v)
    assert not open_, (
        f"{what} must close the new-event form, or opening it again proves nothing; "
        f"{app.driver.diagnose('event-summary')}"
    )


@pytest.mark.feature("drafts-survive")
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
# android joined 2026-09-25: same generic `app.events.*`/`app.driver.type_text`
# calls as every other leg here, and `EventDraftsHost.kt` already lands the
# rail this journey resumes (test_event_draft_persistence.py's own docstring).
@pytest.mark.android
# linux joined 2026-10-05: its dialog closes on Escape, the rail keeps the draft
# and New Event resumes it (`views/events/drafts.rs::resume_draft`).
@pytest.mark.linux
def test_a_half_written_event_is_still_in_the_form_each_time_it_is_opened_again(
    app, request, nest_instance
):
    _fresh_account_with_calendar(app, request, nest_instance)
    ev = app.events
    summary = f"an event I have not finished writing {uuid.uuid4().hex[:8]}"

    # 1. Half-write an event through the opener that resumes.
    ev.open_compose()
    app.driver.type_text("event-summary", summary)
    typed = _poll(ev.compose_summary_text, lambda s: s == summary)
    assert typed == summary, f"precondition: the summary must be in the form, got {typed!r}"

    # 2. Close the form by leaving the page, come back, open it: still there.
    #    Twice — the second opening is the one a first-opener-takes-it app fails.
    for opening in ("first", "second"):
        _leave_and_return(app)
        _assert_form_closed(app, "leaving the Events page")
        ev.open_compose()
        resumed = _poll(ev.compose_summary_text, lambda s: s == summary)
        assert resumed == summary, (
            f"the half-written event must be in the form the {opening} time it is "
            f"opened again after leaving the page (expected {summary!r}, got "
            f"{resumed!r}); error={app.error_text()!r}"
        )


def test_a_day_cell_start_and_a_created_event_leave_the_next_form_blank(
    app, request, nest_instance
):
    """Outcome 12's day-cell and created-event halves. Deliberately NOT yet
    tagged ``drafts-survive``: the outcome also promises that a DISCARDED event
    leaves the next form blank (``events.md`` § Persistence: "A successful
    create, or an explicit discard, clears the rail"), and no app has an
    explicit discard for the event form — ui.yaml's ``event-form`` carries no
    such element. A witness that asserts two of the sentence's three halves is
    not a witness of the sentence, so this test joins the page with the discard
    leg, once that control exists."""
    _fresh_account_with_calendar(app, request, nest_instance)
    ev = app.events
    driver = app.driver
    tag = uuid.uuid4().hex[:8]
    draft = f"an event I was writing before I picked a day {tag}"
    created = f"the event I started from the calendar {tag}"

    # 1. A saved draft, written through New Event and left behind.
    ev.open_compose()
    driver.type_text("event-summary", draft)
    typed = _poll(ev.compose_summary_text, lambda s: s == draft)
    assert typed == draft, f"precondition: the summary must be in the form, got {typed!r}"
    _leave_and_return(app)
    _assert_form_closed(app, "leaving the Events page")

    # 2. Start an event from a day on the month grid: an empty form at that day,
    #    not the saved draft.
    ev.switch_view("month")
    target = date.today().replace(day=15).isoformat()  # always in the month shown
    wait_until(
        lambda: ev.has_day_cell(target),
        UI_SETTLE_S,
        diagnose=lambda: f"precondition: the month grid must paint {target}: "
        f"{driver.diagnose(f'events-day-cell-{target}')}",
    )
    ev.double_click_day_cell(target)
    started = _poll(lambda: _field(driver, "event-dtstart"), lambda s: s.startswith(target))
    assert started.startswith(target), (
        f"a day-cell start must open the form at that day, got event-dtstart "
        f"{started!r}; error={app.error_text()!r}"
    )
    assert ev.compose_summary_text() == "", (
        f"a day-cell start must open an EMPTY form rather than the saved draft, but "
        f"event-summary holds {ev.compose_summary_text()!r}"
    )

    # 3. Write that event on the day it was started from, and create it. The
    #    agenda (every event of the selected calendar) is where it is looked for.
    driver.type_text("event-summary", created)
    driver.type_text("event-dtend", f"{target}T11:00")
    driver.click("create-event")
    ev.navigate()
    listed = _poll(ev.event_summaries, lambda s: any(created in x for x in s),
                   EVENT_CREATE_BUDGET_S)
    assert any(created in x for x in listed), (
        f"precondition: the event started from the day cell must be created, got "
        f"agenda {listed!r}; error={app.error_text()!r}"
    )

    # 4. New Event now opens blank — the created event is not a draft any more.
    #    Every field, not only the summary: the day-cell's own prefill was part
    #    of the event just created.
    ev.open_compose()
    fields = {
        f: _field(driver, f)
        for f in ("event-summary", "event-dtstart", "event-dtend",
                  "event-form-description", "event-form-location")
    }
    assert all(v == "" for v in fields.values()), (
        f"once the event is created, the next new-event form must open blank, got "
        f"{fields!r}; error={app.error_text()!r}"
    )
