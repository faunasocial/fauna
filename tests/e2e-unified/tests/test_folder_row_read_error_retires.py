"""tier_3 e2e: a folder row's own background read error retires once the nest
answers again (``docs/goal/ui/folders.md`` § Errors & edge cases, "A folder row's
own reads retire their own error"; ``transport.md`` § Which surfaces a push
invalidates, the ``devices`` paragraph).

The journey: a folder row is opened while the nest is DOWN, so its background
reads (device activity, place roster, destination places, actor members) fail and
say so on the page's one ``error-message`` bar; the nest returns; the reconnect
sweep (``FfiStaleSurfaces.devices``) resyncs the Devices & Folders machine, the
rebuilt open row re-fires its reads, and each good answer clears the text it put
up — so the bar empties with no user gesture.

What this witness pins and what it does not: the bar emptying after the return is
the end-to-end claim. That a read's good answer leaves a *gesture's* text alone is
pinned beneath it, in ``ErrorBarOwnershipTests`` / ``RowReadErrorsTests`` (the
rules are Core code, so a unit test sees them; a failing gesture inside a
connection gap cannot be made deterministic from here).

android is marked (DevicesVM.rowRead retires a row read's error through
``FfiRowReadErrors``, the same Core rule); its device run is NOT RUN until the
device venue exists.
"""
import secrets

import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
]

# Generous on purpose (test_connection_gap_rules.py's ceilings): a regression
# fails at any ceiling, only load can blow a tight one.
ERROR_WAIT_S = 120.0
RETIRE_WAIT_S = 120.0


@pytest.mark.feature("folders")
def test_a_row_read_error_painted_in_a_nest_gap_retires_after_the_nest_returns(
    logged_in_app, nest_instance
):
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    name = f"rowread-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    nest_down = False
    try:
        stop_nest(nest_instance, graceful=True)
        nest_down = True

        # Opening the row issues its reads against a nest that is not there.
        b.find_and_expand_folder(name)
        wait_until(
            lambda: bool(app.error_text()),
            ERROR_WAIT_S,
            diagnose=lambda: "the row's reads put no error up while the nest was down",
        )
        down_text = app.error_text()
        # Each app words the text its own way, so it is read off the run's output
        # rather than asserted: it must name a row read, not the machine's resync.
        print(f"banner while the nest is down: {down_text!r}")

        start_nest_in_place(nest_instance)
        nest_down = False

        # No gesture: the reconnect sweep alone re-reads and retires the text.
        wait_until(
            lambda: not app.error_text(),
            RETIRE_WAIT_S,
            diagnose=lambda: (
                f"the row read error {down_text!r} outlived the nest's return; "
                f"error surface now: {app.error_text()!r}"
            ),
        )
    finally:
        if nest_down:
            start_nest_in_place(nest_instance)
