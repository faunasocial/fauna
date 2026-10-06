"""The calendar's week start follows the client locale, end to end — linux leg.

`docs/goal/ui/events.md` § Week & day timeline views: *"week-start follows the
client locale (each app via its platform mechanism ...)"*, and its § Where
logic lives amendment (2026-08-01) makes the probe per-**platform**: the two
Rust apps share `fauna_core::caltime::locale_week_start`, which reads
`LC_ALL`/`LC_TIME`/`LANG`.

Sibling of `test_events_locale_week_start.py` (tui), which explains why the
scope is per-platform rather than a parametrized cross-app test: that file's
own docstring already makes the argument, verified to still hold for
linux — linux (GTK) shares tui's `LC_TIME` seam via
the same `fauna_core::caltime::locale_week_start` re-export
(`apps/fauna-linux/src/views/events/time_utils.rs`), so this is the same
platform-probe class, not a fresh one.

**Why the assertion shape differs from tui's.** tui's month grid paints one
composite text label carrying the whole weekday header, so its witness reads
that string. linux's month grid marker (`events-month-grid`) is a bare hidden
1px `Label` used only for AT-SPI lookup — the weekday header itself is
several untagged `Label`s in a `Grid`, not one readable string. What linux
*does* expose per-cell is the indexed `events-day-cell-{YYYY-MM-DD}` marker
(`month_grid.rs`), so this witness pins the same underlying fact — which day
the 6×7 window opens on — through cell presence instead: the grid's
first-drawn day must be visible, and the day immediately before it must not
be, which together pin the exact day the window starts on for whatever month
is showing (no need to fix a specific month, since `fauna_core::caltime::
month_grid`'s window-start math is date-independent).

Convention 14: no wall-clock assertions beyond reading the current date to
compute the expected window (the assertion itself polls no clock).
"""

import datetime

import pytest

from actions import ActionLayer
from conftest import _build_app_config
from drivers import create_driver

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]


# (LC_TIME, caltime's week_start convention: 0=Mon..6=Sun)
_LOCALE_CASES = [
    ("en_GB.UTF-8", 0),
    ("en_US.UTF-8", 6),
]


def _grid_start(year: int, month: int, week_start: int) -> datetime.date:
    """Mirror `fauna_core::caltime::month_grid`'s window-start math.

    Python's `date.weekday()` is 0=Mon..6=Sun — the same convention
    `caltime::day_of_week` remaps to, so no translation is needed.
    """
    first = datetime.date(year, month, 1)
    offset = (first.weekday() - week_start) % 7
    return first - datetime.timedelta(days=offset)


@pytest.mark.parametrize("lc_time,week_start", _LOCALE_CASES)
@pytest.mark.feature("calendar-and-events")
def test_linux_month_grid_opens_on_the_locale_week_start(
    request, nest_instance, test_user, lc_time, week_start
):
    """A linux app launched under `LC_TIME` paints its month grid starting on
    that locale's week-start day.

    Same chain as the tui witness: process env → `caltime::
    locale_week_start` → `month_grid.rs`'s window math → the rendered day
    cells. Break any link and this reddens.
    """
    config = _build_app_config("linux", nest_instance, request)
    config.setdefault("environment", {})
    # LC_ALL outranks LC_TIME in POSIX precedence and the harness may inherit
    # one from the dev box, so clear it and pin LC_TIME explicitly — mirrors
    # the tui witness's own reasoning verbatim.
    config["environment"]["LC_ALL"] = ""
    config["environment"]["LC_TIME"] = lc_time

    driver = create_driver("linux")
    driver.launch(config)
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
                "device_id": "test-device-week-start-linux",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        app = ActionLayer(driver)
        app.events.navigate()
        app.events.switch_view("month")

        today = datetime.date.today()
        grid_start = _grid_start(today.year, today.month, week_start)
        day_before = grid_start - datetime.timedelta(days=1)

        assert app.events.has_day_cell(grid_start.isoformat()), (
            f"LC_TIME={lc_time} (week_start={week_start}) must open the month "
            f"grid on {grid_start.isoformat()}, but that cell is not visible: "
            f"{driver.diagnose(f'events-day-cell-{grid_start.isoformat()}')}"
        )
        assert not app.events.has_day_cell(day_before.isoformat()), (
            f"the day immediately BEFORE the grid's start, "
            f"{day_before.isoformat()}, must not be rendered — if it is "
            f"visible the grid opened a week earlier than LC_TIME={lc_time} "
            "calls for (the locale probe never reached the running app)"
        )
    finally:
        driver.teardown()
