"""The calendar's week start follows the client locale, end to end — android leg.

`docs/goal/ui/events.md` § Week & day timeline views: *"week-start follows the
client locale (each app via its platform mechanism ...)"*. Android's mechanism
is `WeekFields.of(Locale.getDefault()).firstDayOfWeek`
(`apps/fauna-android/.../events/EventsWeekStart.kt`), consumed by the month grid
(`MonthGridContent.kt`) and the week grid. The JVM unit test
`WeekStartRotationTest` pins the rotation maths; what no unit test reaches is the
link before it: that a REAL launch under a chosen locale carries that locale to
`Locale.getDefault()` and the grid opens on that locale's first day. Sibling of
the tui, linux, web and apple legs, kept per-platform for the same reason they
state: each app reads its own platform's locale, none takes orders from another's.

**Driving the seam.** The launch config's optional `"locale"` (a BCP-47 tag) is
applied by `AndroidDriver.set_app_locale` through the platform's own per-app
language setting (`cmd locale set-app-locales`, Android 13+) before the app
starts — the real mechanism, not a `FAUNA_E2E_*` override. The setting persists
per package, so the test clears it in its `finally`.

**Witness status.** Written with no device venue to run it on: recorded
NOT RUN, no green claimed.

The assertion is the apple/linux/web one: the 6x7 window's computed first cell is
visible and the day before it is not. Convention 14: no wall-clock assertions
beyond reading today's date to compute the expected window.
"""

import datetime

import pytest

from actions import ActionLayer
from conftest import _seeded_environment
from drivers import create_driver
from helpers.app_launch import app_launch_config
from helpers.connection import wait_until_online

pytestmark = [pytest.mark.tier_3, pytest.mark.android]

# (BCP-47 tag, caltime's week_start convention: 0=Mon..6=Sun)
_LOCALE_CASES = [
    ("en-GB", 0),
    ("en-US", 6),
]


def _grid_start(year: int, month: int, week_start: int) -> datetime.date:
    """Mirror `fauna_core::caltime::month_grid`'s window-start math (Python's
    `date.weekday()` is already 0=Mon..6=Sun)."""
    first = datetime.date(year, month, 1)
    offset = (first.weekday() - week_start) % 7
    return first - datetime.timedelta(days=offset)


@pytest.mark.parametrize("locale,week_start", _LOCALE_CASES)
@pytest.mark.feature("calendar-and-events")
def test_android_month_grid_opens_on_the_locale_week_start(
    request, nest_instance, test_user, locale, week_start
):
    """An android app launched under a per-app locale paints its month grid
    starting on that locale's week-start day.

    Chain: per-app locale → `Locale.getDefault()` → `localeFirstDayOfWeek()` →
    `MonthGridContent`'s window start → the rendered day cells.
    """
    driver = create_driver("android")
    driver.launch({
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
        **app_launch_config("android", request),
        "locale": locale,
    })
    try:
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": test_user["signing_key"].encode().hex(),
                "handle": test_user.get("handle") or "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-week-start-android",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        # Connection barrier and the MSEK precondition, replicated by hand as the
        # apple leg does (this test owns its driver, so `calendar_backend` and
        # `_login_app_as` do not apply).
        wait_until_online(driver)
        app = ActionLayer(driver)
        app.mail_settings.navigate()
        app.mail_settings.ensure_mail_enabled()
        app.events.navigate()
        app.events.switch_view("month")
        driver.wait_for("events-month-grid", timeout=30)

        today = datetime.date.today()
        grid_start = _grid_start(today.year, today.month, week_start)
        day_before = grid_start - datetime.timedelta(days=1)

        assert app.events.has_day_cell(grid_start.isoformat()), (
            f"android locale={locale} (week_start={week_start}) must open the month "
            f"grid on {grid_start.isoformat()}, but that cell is not visible: "
            f"{driver.diagnose(f'events-day-cell-{grid_start.isoformat()}')}"
        )
        assert not app.events.has_day_cell(day_before.isoformat()), (
            f"the day immediately BEFORE the grid's start, {day_before.isoformat()}, "
            f"must not be rendered — the grid opened a week earlier than android "
            f"locale={locale} calls for (the per-app locale never reached "
            "Locale.getDefault())"
        )
    finally:
        try:
            driver.teardown()
        finally:
            driver.set_app_locale(None)
