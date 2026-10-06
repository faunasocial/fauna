"""The calendar's week start follows the client locale, end to end — windows leg.

`docs/goal/ui/events.md` § Week & day timeline views: *"week-start follows the
client locale (each app via its platform mechanism ...)"*, naming windows'
own mechanism explicitly: `CultureInfo.CurrentCulture.DateTimeFormat.
FirstDayOfWeek` (`FaunaApp.Core.Calendar.WeekStart`). This file closes the
same flow-break gap the tui/linux/web/apple siblings close for their own
platforms: no test drove a REAL launch under a chosen culture and watched a
grid actually open on that locale's first day.

Sibling of `test_events_locale_week_start.py` (tui),
`test_events_locale_week_start_linux.py` (linux),
`test_events_locale_week_start_web.py` (web) and
`test_events_locale_week_start_apple.py` (macos/ios) — same argument for
staying per-platform rather than parametrized cross-app: tui/linux read
`LC_TIME`, web reads the browser's own locale, apple reads `Calendar.current`,
windows reads `CultureInfo.CurrentCulture`, and none of them takes orders
from the others.

**Driving the seam — compile-gated env var, not a launch arg.** Unlike
apple's `-AppleLocale` (a real OS-level launch mechanism Foundation itself
defines), windows has no equivalent per-process locale launch argument for an
unpackaged app (`FaunaApp.csproj`'s `WindowsPackageType=None` — .NET seeds
`CurrentCulture` from the user's Windows regional setting with no per-process
override door). So the seam is `FAUNA_E2E_CULTURE`, read once at the very top
of `App()` (`App.xaml.cs`, before `InitializeComponent()`) and applied to
`CultureInfo.CurrentCulture`/`DefaultThreadCurrentCulture`, compiled out of
release builds (`#if DEBUG || FAUNA_E2E_AGENT`, convention 15 — the
automation surface never ships) — never an ungated CLI arg, which would be a
product-side config knob nothing else needs (`docs/goal/principles.md` § One
configuration surface). `drivers/windows.py` already forwards
`config["environment"]` into the launched process (no driver change needed).

**Why this test owns its driver instead of the cached `app`/`logged_in_app`
fixture** (mirrors `test_account_switcher_windows.py` and the apple leg's own
reasoning): the fixture's launch config is built once per module with no
locale knob, and the culture override must be read at process start, not
toggled after the fact.

**Why the assertion shape targets the WEEK grid, not the month grid.**
windows' month grid (`EventsPage.xaml.cs::BuildMonthGrid`) paints in-month
days only — no leading/trailing out-of-month cells like the tui/linux/web/
apple 42-cell window — so `events-day-cell` presence cannot pin the windows
month grid's first day: the 1st of the month always renders, just in a
different column, and nothing marks which column. The week grid, however,
tags each of its 7 real columns `calendar-day-col-{YYYY-MM-DD}`
(`EventsPage.xaml.cs:654`, spanning `WeekStart.StartOfWeek`), which is
exactly the day-cell-presence idiom the other legs use, one level up: the
computed window-start column must be visible, and the day immediately before
it must not be. `elements.calendar-day-col` in ui.yaml documents this as a
windows-only witness for the same reason.

Convention 14: no wall-clock assertions beyond reading the current date to
compute the expected window (the assertion itself polls no clock).
"""

import datetime

import pytest

from actions import ActionLayer
from conftest import _seeded_environment
from drivers import create_driver
from helpers.connection import wait_until_online

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# (FAUNA_E2E_CULTURE tag, caltime's week_start convention: 0=Mon..6=Sun,
# matching Python's date.weekday() — the same convention WeekStart.cs's
# DayOfWeek arithmetic remaps to via ColumnOf's "+7 before modulo".)
_LOCALE_CASES = [
    ("en-GB", 0),
    ("en-US", 6),
]


def _week_start_date(today: datetime.date, week_start: int) -> datetime.date:
    """Mirror `WeekStart.StartOfWeek`'s arithmetic: the start of the week
    containing `today`, under a locale whose first day is `week_start`."""
    offset = (today.weekday() - week_start) % 7
    return today - datetime.timedelta(days=offset)


@pytest.mark.parametrize("locale,week_start", _LOCALE_CASES)
@pytest.mark.feature("calendar-and-events")
def test_windows_week_grid_opens_on_the_locale_week_start(
    request, nest_instance, test_user, locale, week_start
):
    """A windows app launched under `FAUNA_E2E_CULTURE` paints its week grid
    starting on that locale's week-start day.

    Same chain as the tui/linux/web/apple witnesses, windows' own way:
    process env → `CultureInfo.CurrentCulture` →
    `WeekStart.Current`/`StartOfWeek` → `BuildWeekGrid`'s column math → the
    rendered day columns. Break any link and this reddens.
    """
    driver = create_driver("windows")
    driver.launch({
        "app_path": request.getfixturevalue("windows_app_path"),
        "url": nest_instance["url"],
        "environment": {**_seeded_environment(request, nest_instance), "FAUNA_E2E_CULTURE": locale},
    })
    try:
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": test_user["signing_key"].encode().hex(),
                "handle": test_user.get("handle") or "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": f"test-device-week-start-windows-{locale}",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        # The CONNECTION BARRIER (`conftest._login_app_as`'s own step, load-
        # bearing there): the calendar list is fetched over the just-opened
        # WS-RPC connection, and the events page's calendar section races the
        # handshake if driven before it is online — replicated here since this
        # test owns its driver rather than the fixture chain that normally
        # applies this barrier (mirrors the apple leg).
        wait_until_online(driver)
        app = ActionLayer(driver)
        # `conftest.calendar_backend`'s own precondition, replicated by hand
        # (this test cannot request that fixture directly — see the module
        # docstring): without it the events page's calendar section stays
        # gated behind an unminted MSEK.
        app.mail_settings.navigate()
        app.mail_settings.ensure_mail_enabled()
        app.events.navigate()
        app.events.switch_view("week")
        driver.wait_for("calendar-week-grid", timeout=30)

        today = datetime.date.today()
        week_start_date = _week_start_date(today, week_start)
        day_before = week_start_date - datetime.timedelta(days=1)

        assert driver.is_visible(f"calendar-day-col-{week_start_date.isoformat()}"), (
            f"windows locale={locale} (week_start={week_start}) must open "
            f"the week grid's leftmost column on {week_start_date.isoformat()}, "
            f"but that column is not visible: "
            f"{driver.diagnose(f'calendar-day-col-{week_start_date.isoformat()}')}"
        )
        assert driver.is_absent(f"calendar-day-col-{day_before.isoformat()}"), (
            f"the day immediately BEFORE the week grid's start, "
            f"{day_before.isoformat()}, must not be one of the 7 rendered "
            f"columns — if it is, the grid opened a week earlier than "
            f"windows locale={locale} calls for (FAUNA_E2E_CULTURE never "
            "reached CultureInfo.CurrentCulture, or the fallback masked the "
            "difference between the two cases)"
        )
    finally:
        driver.teardown()
