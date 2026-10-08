"""tier_3 — on iOS and android the widget's count keeps itself current through the OS-scheduled refresh.

`apps/common.md` § Home-screen widget promises the count stays current in the
background without the app being opened; `apps/ios.md` § Home-screen widget names
the iOS mechanism: iOS suspends a backgrounded app, so the
`social.fauna.widget.refresh` `BGAppRefreshTask` runs one conversations receive
pass (`ConversationsVM.receivePass` — the session's `pollConversations` +
`pollMail` backstop), whose ingest ticks the observer that publishes the count.
This file is `docs/features/home-screen-widget.md` outcome 2's iOS witness; the
macOS one (the app resident with its window closed) is
`test_home_screen_widget.py`.

**android** has the same shape (`apps/android.md` § App Widgets): WorkManager
alone schedules the 15-minute `WidgetDataWorker`, whose pass is one receive pass
over the live session (`ConversationsManagerHost.widgetRefreshPass`), and the
poke enqueues that same worker through WorkManager — the schedule's own
construct-and-run path, never the body called around it.

**How a schedule only the OS owns is driven.** No test can wait for a
`BGAppRefreshTask`, and the type has no public initializer, so the handler's body
is `BackgroundScheduler.runWidgetRefreshPass` and
`fauna_e2e_agent::WIDGET_REFRESH_SCHEDULED_PASS_NOW` pokes it — convention 14's
`run_now`, the same method the OS calls, never a shortcut around it (the photo-
backup precedent, `test_photo_backup_unattended.py`).

**What this asserts, and what it deliberately does not.** It asserts the pass the
poke caused (the `widget_refresh` counters are bumped by that method and nothing
else) reached a LIVE conversations session — `last_pass_count` is `null` when the
pass had nothing to poll, which is exactly what a mock-backend or unwired session
would leave, hence `real_conversations` — and that the count it left behind is
the app's own list total, with an unread planted before the poke, and is the
number on the widget's snapshot. It does NOT attribute the planted message's
*delivery* to the pass: with the app in the foreground the receive loop and the
observer are live, so no arrival can be made to wait for a poll alone. That is the
considered choice the photo-backup scheduled-pass witness makes too — what only
this test can witness is that the scheduled entry point is WIRED to the real
receive pass, the failure a registered-but-inert task would otherwise hide for
ever (and one this row found: the e2e real-session login never configured the
pass at all).

`real_conversations` is session-wide (`_apply_real_conversations_env`), so run
this module in its own invocation.
"""

from __future__ import annotations

import pytest

from actions.conversations import ConversationsActions
from helpers.budgets import RECEIVE_CYCLE_S
from helpers.home_screen_widget import (
    WIDGET_REFRESH_SCHEDULED_PASS_NOW,
    app_unread_total,
    await_widget_converged,
    plant_unread,
    widget_refresh_passes,
    widget_snapshot,
)
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # iOS and android, by design: the apps whose widget currency rests on an
    # OS-scheduled task (`feature-catalog.md` § Cell semantics, the
    # marked-witness rule). macOS refuses the poke — it has no scheduler.
    pytest.mark.ios,
    pytest.mark.android,
]


@pytest.mark.feature("home-screen-widget")
def test_the_scheduled_background_refresh_keeps_the_count_current(real_faunamls_app):
    """The widget's `BGAppRefreshTask` body polls the live session and leaves the
    widget showing the list's own count."""
    driver = real_faunamls_app.driver
    conv = ConversationsActions(driver)

    # The baseline the attribution rests on, read BEFORE anything changes.
    completed_before = widget_refresh_passes(driver)["passes_completed"]

    before = app_unread_total(conv)
    plant_unread(conv, "background")
    shown = await_widget_converged(driver, conv, above=before)

    # Convention 14's run_now poke. Fire-and-forget by contract — the barrier is
    # the pass counter below, never this ack.
    driver.call_command(WIDGET_REFRESH_SCHEDULED_PASS_NOW)

    passes = wait_until(
        lambda: (
            p if (p := widget_refresh_passes(driver))["passes_completed"] > completed_before
            else None
        ),
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"no widget-refresh pass completed after the "
            f"{WIDGET_REFRESH_SCHEDULED_PASS_NOW} poke: counters "
            f"{widget_refresh_passes(driver)!r} against a completed baseline of "
            f"{completed_before}"
        ),
    )

    counted = passes["last_pass_count"]
    assert counted is not None, (
        "the scheduled widget refresh ran but had no conversations session to "
        "poll — the BGAppRefreshTask is wired to nothing, so on a real phone the "
        f"widget would never move in the background. Counters: {passes!r}"
    )
    total = app_unread_total(conv)
    assert counted == total, (
        f"the pass left the widget count at {counted}, but the app's own list "
        f"shows {total} (the planted unread made it {shown}) — the pass is not "
        "reading the list's own number"
    )
    snap = widget_snapshot(driver)
    assert snap is not None and snap.get("count") == total, (
        f"after the scheduled refresh the widget's snapshot reads {snap!r}, not "
        f"the app's own unread total {total}"
    )
