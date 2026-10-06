"""The calendar's week start follows the client locale, end to end.

`docs/goal/ui/events.md` § Week & day timeline views: *"week-start follows the
client locale (each app via its platform mechanism ...)"*, and its § Where logic
lives amendment (2026-08-01) makes the probe per-**platform**: the two Rust apps
share `fauna_core::caltime::locale_week_start`, which reads `LC_ALL`/`LC_TIME`/
`LANG`.

**Why this file exists, and why it is tui-only.** The tier_1 pins in
`apps/fauna-tui/src/events/mod.rs` set `EventsState.week_start` directly, so
they prove the *renderers* honour it — the rotation, and that the range label
agrees with the grid. What no unit test can reach is the link *before* them:
that `events::init` actually calls the probe, and that the probe actually reads
the process environment of a real launch. That is the flow break a
symbol-existence check misses — the links *between* named symbols — and driving
it needs a real app launched under a chosen locale.

Only tui is driven here, and that is a **declared** scope rather than debt: the
env var *is* tui's (and linux's) platform mechanism, while windows reads
`CultureInfo`, apple reads `Calendar.firstWeekday`, android reads `WeekFields`
and web reads `Intl` — none of which take orders from `LC_TIME`. A generic
parametrized version of this test would be asserting that four apps ignore an
environment variable, which is not the behaviour anyone wants pinned.

Convention 14: no wall-clock assertions. The observable is the rendered grid,
reached through the action layer's own existing barrier
(`switch_view` waits on the `events-month-grid` marker).
"""

import pytest

from actions import ActionLayer
from conftest import _build_app_config
from drivers import create_driver

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]


# (LC_TIME, the weekday the month grid's first column must be)
#
# July 2026's 1st is a Wednesday, so each locale's 42-cell grid opens on a
# different day of the preceding week — which is what makes the first cell a
# sharper witness than the header alone.
_LOCALE_CASES = [
    ("en_GB.UTF-8", "Mon"),
    ("en_US.UTF-8", "Sun"),
]


@pytest.mark.parametrize("lc_time,expected_first_column", _LOCALE_CASES)
@pytest.mark.feature("calendar-and-events")
def test_tui_month_grid_opens_on_the_locale_week_start(
    request, nest_instance, test_user, tui_app_path, lc_time, expected_first_column
):
    """A tui launched under `LC_TIME` paints its month grid starting on that
    locale's week-start day.

    This is the whole chain in one assertion: process env → `caltime::
    locale_week_start` → `events::init` → `EventsState.week_start` →
    `render_month`'s weekday header. Break any link — revert the const, drop the
    probe from `init`, stop rotating the header — and this reddens, which is
    exactly what the tier_1 pins cannot see.
    """
    config = _build_app_config("tui", nest_instance, request)
    config.setdefault("environment", {})
    # LC_ALL outranks LC_TIME in POSIX precedence and the harness may inherit one
    # from the dev box (this machine exports LANG=en_US.UTF-8), so clear it and
    # pin LC_TIME explicitly — otherwise the ambient locale, not the parameter,
    # decides the result and both cases would silently assert the same thing.
    config["environment"]["LC_ALL"] = ""
    config["environment"]["LC_TIME"] = lc_time

    driver = create_driver("tui")
    driver.launch(config)
    try:
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": test_user["signing_key"].encode().hex(),
                # The actor's REAL handle when it has one, matching
                # conftest._login_app_as — a fabricated handle breaks anything
                # that derives `<handle>@<domain>`.
                "handle": test_user.get("handle") or "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-week-start",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        app = ActionLayer(driver)
        app.events.navigate()
        app.events.switch_view("month")

        header = driver.get_text("events-month-grid")
        assert header, (
            "the month grid registers its marker with the weekday header as its "
            f"text: {driver.diagnose('events-month-grid')}"
        )
        assert header.split()[0] == expected_first_column, (
            f"LC_TIME={lc_time} must open the week on {expected_first_column}; "
            f"the grid header reads {header!r}. A wrong value here means the "
            "locale probe never reached the running app — the grid and its "
            "range label are pinned separately at tier_1."
        )
    finally:
        driver.teardown()
