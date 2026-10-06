"""The apple home-screen widget shows the app's own unread count, and keeps it current.

`apps/common.md` § Home-screen widget: a Fauna widget on the platform's home
screen shows how many unread messages are waiting — **the number the app's own
conversations list shows**, the shared `fauna_conversations` fold summed, never a
per-app tally or a second count query — and keeps that count current in the
background without the app being opened.

**How the widget is observed.** A widget lives outside the app's automation
surface, so these tests never ask the app what its widget shows. The apple
widget is a sandboxed extension holding no credential; it renders the snapshot
the app publishes for it (`WidgetUnreadPublisher` → `UnreadSnapshotStore`), so
the witness reads that snapshot straight off disk, from outside the app
(`helpers.home_screen_widget`). What the widget paints for a given snapshot is
pinned headlessly beside the widget code (`HomeScreenWidgetTests` under
`just swift-test`); these tests prove the app's own number reaches the file the
widget reads, and moves with it.

**The oracle is the app's own list, never a constant** — the linux witness's
shape (`test_home_screen_widget_launcher_badge.py`): the session-scoped account
may already hold unread threads from earlier tests, so every assertion compares
the snapshot against the app's thread list summed at that moment.

**Marked `macos` / `ios` — the apps whose driver can read the snapshot**
(`feature-catalog.md` § Cell semantics, the marked-witness rule). Outcome 2's
test here is `macos` only: macOS keeps the app — and so the conversations
observer that publishes — running with its window closed, which is exactly what
it proves. iOS's background leg is a `BGAppRefreshTask` the OS alone schedules,
so a foreground run here would prove the wrong mechanism; its witness is
`test_home_screen_widget_background_refresh.py`, which drives that task's
production body through the convention-14 poke.
"""

from __future__ import annotations

import pytest

from actions import ActionLayer
from actions.conversations import ConversationsActions
from common.launch_harness import reached_authenticated_app
from conftest import _login_app_as, _seeded_environment
from drivers import create_driver
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.home_screen_widget import (
    app_unread_total,
    await_widget_converged,
    plant_unread,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_2


@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
@pytest.mark.ios
def test_the_widget_shows_the_apps_own_unread_count(logged_in_app):
    """A message you have not read → the widget's number is the list's total."""
    conv = ConversationsActions(logged_in_app.driver)
    before = app_unread_total(conv)
    plant_unread(conv, "one")
    await_widget_converged(logged_in_app.driver, conv, above=before)


@pytest.fixture
def macos_app_alone(request, nest_instance, test_user):
    """A logged-in macOS app of this test's own: the test closes its window,
    which must not strand the session-shared app every later test drives."""
    driver = create_driver("macos")
    try:
        driver.launch({
            "url": nest_instance["url"],
            "app_path": request.getfixturevalue("macos_app_path"),
            "environment": _seeded_environment(request, nest_instance),
        })
        app = ActionLayer(driver)
        _login_app_as(app, request, nest_instance, test_user)
        yield app
    finally:
        driver.teardown()


@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
def test_the_count_keeps_itself_current_without_opening_the_app(macos_app_alone):
    """The window closed, nothing on screen — a new arrival still reaches the widget.

    On macOS a window close is never a quit (`apps/macos.md` § Sync), so the app,
    its receive path and the conversations observer that publishes the count all
    keep running; that residency is what "without opening the app" rests on here.
    """
    driver = macos_app_alone.driver
    conv = ConversationsActions(driver)

    before = app_unread_total(conv)
    plant_unread(conv, "open")
    shown = await_widget_converged(driver, conv, above=before)

    driver.window_close()
    assert driver.is_app_alive(), "a macOS window close must keep the app running"

    plant_unread(conv, "closed")
    await_widget_converged(driver, conv, above=shown)


@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
def test_an_autostart_launch_is_resident_with_no_window_and_keeps_the_count_current(
    macos_app_alone,
):
    """The sign-in leg of "without opening the app": the login launch comes up
    signed in with no main window, and a new arrival still reaches the widget.

    `apps/macos.md` § App Lifecycle → *Auto-start at sign-in*: the registered
    LaunchAgent starts the app with `--autostart`, and an auto-start launch that
    lands authenticated is resident — menu bar, no window. The OS registration
    itself is gated off under e2e (its truth table is `AutoStartTests` under
    `just swift-test`); this drives the launch the job would make, argv and all.
    """
    driver = macos_app_alone.driver
    assert driver.preserve_state_across_relaunch(), (
        "the relaunch must come back signed in, or it routes to onboarding — "
        "which is loud by design"
    )
    assert driver.relaunch_with_args("--autostart"), "the --autostart relaunch did not come up"
    reached_authenticated_app(driver, timeout=90)

    wait_until(
        lambda: driver.visible_windows() == [] or None,
        RPC_ROUNDTRIP_S,
        interval=0.3,
        diagnose=lambda: f"main windows still on screen: {driver.visible_windows()!r}",
    )
    assert driver.is_app_alive(), "the hidden auto-start launch must stay resident"

    conv = ConversationsActions(driver)
    before = app_unread_total(conv)
    plant_unread(conv, "autostart")
    await_widget_converged(driver, conv, above=before)
    assert driver.visible_windows() == [], "a new arrival must not raise the window"
