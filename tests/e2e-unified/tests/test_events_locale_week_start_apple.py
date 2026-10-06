"""The calendar's week start follows the client locale, end to end — apple leg.

`docs/goal/ui/events.md` § Week & day timeline views: *"week-start follows the
client locale (each app via its platform mechanism ...)"*, naming apple's own
mechanism explicitly: `Calendar.current.firstWeekday`. Its § Where logic lives
amendment (2026-08-01) makes the probe per-**platform**, and apple is one of
the five with a locale-aware platform calendar library — § Implementation
status today already records both apple targets as ✅ *"already correct via
`Calendar.current.firstWeekday`"* at the unit level (`MonthGridView.swift:130`
rotates on `calendar.firstWeekday`, shared FaunaKit between macOS and iOS).
This file exists to close the same flow-break gap the tui/linux/web siblings
close for their own platforms: no test drove a REAL launch under a chosen
locale and watched the grid actually open on that locale's first day — the
link between "the code reads `Calendar.current`" and "a real process launch
carries the locale that answer needs" is exactly what a unit test cannot see.

Sibling of `test_events_locale_week_start.py` (tui),
`test_events_locale_week_start_linux.py` (linux) and
`test_events_locale_week_start_web.py` (web) — same argument for staying
per-platform rather than parametrized cross-app: tui/linux read `LC_TIME`, web reads the
browser's own locale, apple reads `Calendar.current`, and none of them takes
orders from the others.

**Driving the seam.** Both apple apps launch through the standard Foundation
argument-domain mechanism, `-AppleLocale <tag>` — the same technique Xcode's
own scheme "App Region" option and XCUITest's `launchArguments` use, and the
one that actually flips `Locale.current` → `Calendar.current.firstWeekday`
(a `FAUNA_E2E_*` env override would witness a test seam instead of the real
mechanism, and add a product-side config knob nothing else needs — this
project's one-configuration-surface rule: every non-constant knob is either a
user/admin choice in the app UI or a hard-coded constant, never a hand-edited
env/flag). macOS needed no driver
change: `MacosInProcessDriver.launch` already threads `config["app_args"]`
through `shlex.split` into the Popen argv. iOS needed one: `drivers/ios.py`'s
`simctl launch` call did not forward any argv to the launched process — it now
appends `shlex.split(config.get("app_args", ""))` after the bundle id, mirroring
macOS.

**Why this test owns its driver instead of the cached `app`/`logged_in_app`
fixture** (mirrors `test_account_switcher_apple.py` and
`test_events_locale_week_start_web.py`'s own reasoning): the fixture's launch
config is built once per module with no locale knob, and `-AppleLocale` must
be set at process-launch time, not toggled after the fact. **iOS carries one
extra wrinkle** (`drivers/ios.py`'s `launch()`): a plain launch terminates and
uninstalls whatever was previously on the shared ephemeral-simulator UDID, so
this test cannot ride any state a fixture-launched iOS app left behind — every
precondition (login, mail-enable) is replicated by hand below, exactly as the
web leg replicates `calendar_backend`'s dance for the same reason. This file
being its own pytest module means the harness's per-module cold relaunch
(convention 10) covers any *later* module that uses the cached `app` fixture
on the same UDID — no state leaks across the boundary.

**Why the assertion shape matches linux's and web's, not tui's.** apple's
month grid is a real per-cell grid (`MonthGridView.swift`'s
`events-day-cell-{YYYY-MM-DD}` markers, shared FaunaKit between macOS and
iOS), not tui's single composite text label, so this pins the grid's exact
first day via day-cell presence: the computed window-start cell must be
visible, and the day immediately before it must not be.

Convention 14: no wall-clock assertions beyond reading the current date to
compute the expected window (the assertion itself polls no clock).
"""

import datetime

import pytest

from actions import ActionLayer
from conftest import _seeded_environment
from drivers import create_driver
from helpers.connection import wait_until_online

pytestmark = [pytest.mark.tier_3, pytest.mark.macos, pytest.mark.ios]

APPS = ["macos", "ios"]


@pytest.fixture
def app_name(request):
    """The app this item drives — an INDIRECT parametrization on purpose. A
    direct ``parametrize("app_name", …)`` is a pytest pseudo-fixture, which
    ``conftest._parametrized_clients`` deliberately ignores, so the item fell
    back to this module's ``[macos, ios]`` marks: ``--app macos`` kept every
    ``[ios]`` item (whose app it never prebuilt) and ``--app ios`` kept every
    macOS-only one. A real fixture makes the parametrization the deselection
    key, like every ``app``-fixture test."""
    return request.param

# (-AppleLocale tag, caltime's week_start convention: 0=Mon..6=Sun)
_LOCALE_CASES = [
    ("en_GB", 0),
    ("en_US", 6),
]


def _grid_start(year: int, month: int, week_start: int) -> datetime.date:
    """Mirror `fauna_core::caltime::month_grid`'s window-start math — the same
    computation the linux/web witnesses use, since every app's month grid
    paints the same 6x7 window shape for a given `week_start`.

    Python's `date.weekday()` is 0=Mon..6=Sun — the same convention
    `caltime::day_of_week` remaps to, so no translation is needed.
    """
    first = datetime.date(year, month, 1)
    offset = (first.weekday() - week_start) % 7
    return first - datetime.timedelta(days=offset)


def _apple_launch_config(app_name, request, locale):
    """App-specific keys `create_driver(app_name).launch()` needs, plus the
    `-AppleLocale` launch argument both platforms honour identically. macOS
    wants only `app_path`; iOS additionally needs `udid` (the simctl target).
    Fetched via `request.getfixturevalue` (not a plain fixture parameter) so a
    `--app macos` run never triggers the iOS build, and vice versa — mirrors
    `test_account_switcher_apple.py::_apple_launch_config`."""
    app_args = f"-AppleLocale {locale}"
    if app_name == "ios":
        setup = request.getfixturevalue("ios_setup")
        return {"app_path": setup["app_path"], "udid": setup["udid"], "app_args": app_args}
    return {"app_path": request.getfixturevalue("macos_app_path"), "app_args": app_args}


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.parametrize("locale,week_start", _LOCALE_CASES)
@pytest.mark.feature("calendar-and-events")
def test_apple_month_grid_opens_on_the_locale_week_start(
    request, nest_instance, test_user, app_name, locale, week_start
):
    """A macOS/iOS app launched under `-AppleLocale` paints its month grid
    starting on that locale's week-start day.

    Same chain as the tui/linux/web witnesses, apple's own way: process argv
    → `Locale.current` → `Calendar.current.firstWeekday` →
    `MonthGridView.weeks`'s window math → the rendered day cells. Break any
    link and this reddens.
    """
    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
        **_apple_launch_config(app_name, request, locale),
    })
    try:
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": test_user["signing_key"].encode().hex(),
                # The actor's REAL handle when it has one, matching
                # conftest._login_app_as — a fabricated handle breaks
                # anything that derives `<handle>@<domain>`.
                "handle": test_user.get("handle") or "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": f"test-device-week-start-{app_name}",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        # The CONNECTION BARRIER (`conftest._login_app_as`'s own step, load-
        # bearing there): the calendar list is fetched over the just-opened
        # WS-RPC connection, and the events page's calendar section races the
        # handshake if driven before it is online — same reasoning as the web
        # leg, replicated here since this test owns its driver rather than the
        # fixture chain that normally applies this barrier.
        wait_until_online(driver)
        app = ActionLayer(driver)
        # `conftest.calendar_backend`'s own precondition, replicated by hand
        # (this test cannot request that fixture directly — see the module
        # docstring): without it the events page's calendar section stays
        # gated behind an unminted MSEK. Mints the actor's MSEK the encrypted
        # CalDAV store needs.
        app.mail_settings.navigate()
        app.mail_settings.ensure_mail_enabled()
        app.events.navigate()
        app.events.switch_view("month")
        driver.wait_for("events-month-grid", timeout=30)

        today = datetime.date.today()
        grid_start = _grid_start(today.year, today.month, week_start)
        day_before = grid_start - datetime.timedelta(days=1)

        assert app.events.has_day_cell(grid_start.isoformat()), (
            f"{app_name} locale={locale} (week_start={week_start}) must open "
            f"the month grid on {grid_start.isoformat()}, but that cell is "
            f"not visible: "
            f"{driver.diagnose(f'events-day-cell-{grid_start.isoformat()}')}"
        )
        assert not app.events.has_day_cell(day_before.isoformat()), (
            f"the day immediately BEFORE the grid's start, "
            f"{day_before.isoformat()}, must not be rendered — if it is "
            f"visible the grid opened a week earlier than {app_name} "
            f"locale={locale} calls for (the -AppleLocale launch argument "
            "never reached Calendar.current, or the fallback masked the "
            "difference between the two cases)"
        )
    finally:
        driver.teardown()
