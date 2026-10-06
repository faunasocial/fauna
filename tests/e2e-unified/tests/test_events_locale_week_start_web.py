"""The calendar's week start follows the client locale, end to end — web leg.

`docs/goal/ui/events.md` § Week & day timeline views: *"week-start follows the
client locale (each app via its platform mechanism ...)"*, and its § Where
logic lives amendment (2026-08-01) makes the probe per-**platform**. Web is
the deliberate exception among the per-platform legs (`weekStart.ts`'s own
module doc): it must NOT consume the shared Rust `caltime::locale_week_start`
probe, because the browser already carries the full CLDR `weekData` the Rust
table only approximates — routing through wasm would hand web a worse answer
than the one under its feet. Web reads `navigator.language` →
`Intl.Locale.getWeekInfo().firstDay`, converted to caltime's `0 = Mon`
convention by `weekStartFromIntlFirstDay`.

Sibling of `test_events_locale_week_start.py` (tui) and
`test_events_locale_week_start_linux.py` (linux) — same argument for staying
per-platform rather than parametrized cross-app:
tui/linux read `LC_TIME`, web reads the browser's own locale, and neither
takes orders from the other.

**Driving the seam.** Nothing in the harness previously exposed the browser
context's locale, since no earlier web test needed to control
`navigator.language`. The web bridge's `/session` now accepts an optional
`"locale"` field (a BCP-47 tag, e.g. `"en-GB"`), threaded into Playwright's
`new_context(locale=...)` — absent by default, so every other caller is
unaffected (`web-bridge/server.py::_handle_session_create`,
`drivers/web.py::WebDriver.launch`).

**Why the assertion shape matches linux's, not tui's.** Like linux, web's
month grid is a real per-cell grid (`events-day-cell-{YYYY-MM-DD}` markers,
`docs/features/calendar-and-events.md`'s parity table) rather than one
composite text label, so this pins the grid's exact first day via day-cell
presence: the computed window-start cell must be visible, and the day
immediately before it must not be.

Convention 14: no wall-clock assertions beyond reading the current date to
compute the expected window (the assertion itself polls no clock).
"""

import datetime

import pytest

from actions import ActionLayer
from conftest import _build_app_config
from drivers import create_driver
from helpers.connection import wait_until_online

pytestmark = [pytest.mark.tier_3, pytest.mark.web]


# (BCP-47 locale, caltime's week_start convention: 0=Mon..6=Sun)
_LOCALE_CASES = [
    ("en-GB", 0),
    ("en-US", 6),
]


def _grid_start(year: int, month: int, week_start: int) -> datetime.date:
    """Mirror `fauna_core::caltime::month_grid`'s window-start math — the same
    computation `test_events_locale_week_start_linux.py` uses, since both
    apps' grids paint the same 6x7 window shape for a given `week_start`.

    Python's `date.weekday()` is 0=Mon..6=Sun — the same convention
    `caltime::day_of_week` remaps to, so no translation is needed.
    """
    first = datetime.date(year, month, 1)
    offset = (first.weekday() - week_start) % 7
    return first - datetime.timedelta(days=offset)


@pytest.mark.parametrize("locale,week_start", _LOCALE_CASES)
@pytest.mark.feature("calendar-and-events")
def test_web_month_grid_opens_on_the_locale_week_start(
    request, nest_instance, test_user, spa_url, locale, week_start
):
    """A web session opened under a browser `locale` paints its month grid
    starting on that locale's week-start day.

    Same chain as the tui/linux witnesses, web's own way: browser context
    locale → `navigator.language` → `Intl.Locale.getWeekInfo` →
    `weekStart.ts::localeWeekStart` → the rendered day cells. Break any link
    and this reddens.
    """
    config = _build_app_config("web", nest_instance, request)
    config["locale"] = locale

    driver = create_driver("web")
    driver.launch(config)
    try:
        driver.set_state({
            "session": {
                "authenticated": True,
                # The CORS-adding SPA proxy URL, NOT `nest_instance["url"]`
                # directly — the browser fetches relative to it
                # (`conftest._login_app_as`'s own doc comment).
                "node_url": spa_url,
                "secret_hex": test_user["signing_key"].encode().hex(),
                # The actor's REAL handle when it has one, matching
                # conftest._login_app_as — a fabricated handle breaks
                # anything that derives `<handle>@<domain>`.
                "handle": test_user.get("handle") or "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-week-start-web",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        # The CONNECTION BARRIER (`conftest._login_app_as`'s own step, load-
        # bearing there): the calendar list is fetched over the just-opened
        # WS-RPC connection, and the events page's month grid renders only
        # once `calendars.length > 0` — proceeding before the transport is
        # online races the handshake and the grid never mounts at all
        # (measured: `events-month-grid` count=0, not a wrong-locale day).
        wait_until_online(driver)
        app = ActionLayer(driver)
        # `conftest.calendar_backend`'s own precondition, replicated by hand:
        # this test builds its own driver (for the locale override) rather
        # than the shared cached `app` fixture, so it cannot request that
        # fixture directly. Without it web's events page gates its ENTIRE
        # calendar section (month grid included) behind `calendars.length >
        # 0` — a page-level divergence from linux/tui, which paint an empty
        # grid shell regardless (measured: without this the grid never
        # mounts at all, `events-month-grid` count=0, not a wrong-locale
        # day). Mints the actor's MSEK the encrypted CalDAV store needs.
        app.mail_settings.navigate()
        app.mail_settings.ensure_mail_enabled()
        app.events.navigate()
        app.events.switch_view("month")
        # `switch_view`'s own wait for this marker is a short, silently-caught
        # `wait_for` (falls through on timeout so a *different* assertion
        # reports the real failure) — under machine load that budget can be
        # too short for genuinely unrelated reasons. Wait again here, with a
        # generous budget and a raised (not swallowed) TimeoutError, so a
        # grid that never mounted at all fails with an unambiguous message
        # instead of masquerading as a wrong-locale day-cell miss.
        driver.wait_for("events-month-grid", timeout=30)

        today = datetime.date.today()
        grid_start = _grid_start(today.year, today.month, week_start)
        day_before = grid_start - datetime.timedelta(days=1)

        assert app.events.has_day_cell(grid_start.isoformat()), (
            f"locale={locale} (week_start={week_start}) must open the month "
            f"grid on {grid_start.isoformat()}, but that cell is not visible: "
            f"{driver.diagnose(f'events-day-cell-{grid_start.isoformat()}')}"
        )
        assert not app.events.has_day_cell(day_before.isoformat()), (
            f"the day immediately BEFORE the grid's start, "
            f"{day_before.isoformat()}, must not be rendered — if it is "
            f"visible the grid opened a week earlier than locale={locale} "
            "calls for (the locale probe never reached the running app, or "
            "the browser's Intl.Locale has no weekInfo and the fallback "
            "masked the difference between the two cases)"
        )
    finally:
        driver.teardown()
