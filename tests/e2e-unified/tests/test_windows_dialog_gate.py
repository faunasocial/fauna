"""tier_3 E2E: a second dialog opened while one is up is refused, not a crash.

WinUI allows ONE ``ContentDialog`` per XamlRoot, and ``ShowAsync()`` throws
``InvalidOperationException`` on a second. Every show handler is ``async void``,
so before the shared gate that throw reached ``Application.UnhandledException``
and killed the process (``[fauna] FATAL (UI thread) Only a single ContentDialog
can be open at any time.``) — observed when one test left the Backups add
dialog open and the next pressed add again, losing every later test in the
file to ``BridgeDead``.

The windows shell now routes every ``ShowAsync`` through one helper
(``apps/fauna-windows/FaunaApp/FaunaApp/Controls/Dialogs.cs``) that refuses the
second open legibly on ``error-message`` (e2e convention 11: never a silent
drop) and keeps the open dialog up. The hazard is WinUI's own, so this file is
windows-only: GTK4, SwiftUI, Compose, the web SPA and the terminal UI have no
one-dialog-per-window throw.
"""

import pytest

from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

ADD = "backup-destination-add-button"
URL_INPUT = "backup-destination-url-input"
CANCEL = "backup-destination-add-cancel-button"


def test_a_second_dialog_open_is_refused_and_the_app_stays_alive(logged_in_app):
    app = logged_in_app
    app.backups.require_destination_management_supported()
    app.backups.navigate()

    # First open: the add dialog comes up.
    app.backups.open_add_destination()
    app.driver.clear_and_type(URL_INPUT, "https://kept.example")

    # Second open while the first is still up — the path that used to throw
    # out of an async void handler and take the process down.
    app.driver.click(ADD)

    # Alive, and the refusal is legible rather than silent. Polled: the click
    # returns before the handler runs, and on an ungated build the process dies
    # a moment later (the read then raises AppExitedException).
    wait_until(
        lambda: app.error_text() == S.common.dialog_already_open,
        10,
        diagnose=lambda: f"a second dialog open must be refused on error-message; "
        f"error={app.error_text()!r}",
    )
    # The first dialog is untouched: still open, the user's input intact.
    assert app.driver.is_visible(URL_INPUT)
    assert app.driver.get_text(URL_INPUT) == "https://kept.example"

    app.driver.click(CANCEL)
    wait_until(lambda: app.driver.is_absent(URL_INPUT), 10)


# The incident's own shape: one test ends with a dialog up, and the next test's
# reset() must close it — every dialog is in the gate's registry, which reset
# force-closes, so no page can leave one behind that it did not register.
# Order-dependent by design (pytest runs a module's tests in definition order,
# on the one app process `logged_in_app` reuses across the module).


def test_a_test_may_end_with_a_dialog_open(logged_in_app):
    app = logged_in_app
    app.backups.require_destination_management_supported()
    app.backups.navigate()
    app.backups.open_add_destination()


def test_reset_closes_the_dialog_the_previous_test_left_open(logged_in_app):
    app = logged_in_app
    app.backups.require_destination_management_supported()
    # Not `is_absent(URL_INPUT)` first: a stale dialog whose page was navigated
    # away is invisible to UIA (measured 2026-09-27), so that read is vacuous.
    # The witness is the open itself — with the stale dialog still up it is
    # refused (mutation-proved: CloseAll made a no-op reds right here).
    app.backups.navigate()
    app.backups.open_add_destination()
    assert app.error_text() == "", f"error={app.error_text()!r}"
    app.driver.click(CANCEL)
    wait_until(lambda: app.driver.is_absent(URL_INPUT), 10)
