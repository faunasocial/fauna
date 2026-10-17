"""tier_3 e2e: an evicted member is told — the eviction warning reaches the
member's every-page banner, and the admin calling it off clears it.

Goal docs: ``docs/goal/behavior/admin.md`` § Cutting a user off — eviction and
suspension → *The member is told* (what the alert says and when it is raised
and cleared); ``docs/goal/behavior/critical-alerts.md`` § Feeders (key
``eviction:<actor_id>``, raised from the member's own ``fauna.account.get``)
and § Implementation status today (swept like the others, no faster trigger).

**Two seats, one nest.** The member's seat is the app under test — the one
whose banner is the claim. The admin's seat is a second ``tui`` launch, the
lead app, whose Users page is the admin's real eviction surface: the evict and
the cancel are UI clicks there (convention 8), never an RPC standing in for the
admin. Making the admin seat fixed and the member seat the parametrized app is
what lets this one journey witness all seven apps' banners without a per-app
launcher.

**Anchored on the sweep's own pass counters, never a sleep (convention 14).**
The member's seat runs the production re-sweep loop; the journey ends its
six-hour wait with the ``alert_sweep_wake`` poke and then waits for a pass that
*began after* the admin's click (``await_sweep_pass_after``), so "the banner is
up" and "the banner is down" are both read after a pass that saw the change.

tier_3: needs a real ``fauna-nest`` binary and a ``tui`` build for the admin.
"""
from __future__ import annotations

import re
from datetime import datetime, timedelta

import pytest

from helpers.budgets import ALERT_SWEEP_PASS_S
from helpers.waiting import alert_sweep_passes, await_sweep_pass_after, wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

CRITICAL_ALERTS = "critical-alerts"
CRITICAL_ALERT = "critical-alert"
# `fauna_e2e_agent::ALERT_SWEEP_WAKE` — end the member seat's re-sweep wait so
# the production loop sweeps again.
ALERT_SWEEP_WAKE = "alert_sweep_wake"

# The headline's fixed opening, from the shared string every app renders — the
# countdown and the two dates are the moving parts after it.
_HEAD = S.critical_alerts.eviction_warning(days="\0", suspend_at="", delete_at="").split("\0")[0]
_UTC_MINUTE = re.compile(r"(\d{4}-\d{2}-\d{2} \d{2}:\d{2}) UTC")


def _alert_text(driver) -> str:
    """Every active alert row's text joined, or "" when the banner is absent —
    presence of the banner IS the rendering contract (`critical-alerts.md`
    § Rendering contract)."""
    if not driver.is_visible(CRITICAL_ALERTS):
        return ""
    return "\n".join(driver.get_texts(CRITICAL_ALERT))


def _poll_banner(driver, *, present: bool, budget_s: float = ALERT_SWEEP_PASS_S) -> str:
    """Deadline-poll until the eviction headline's presence is ``present``."""
    import time

    deadline = time.monotonic() + budget_s
    text = _alert_text(driver)
    while (_HEAD in text) != present and time.monotonic() < deadline:
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations, not a settle-wait
        text = _alert_text(driver)
    return text


def _wake_and_await_pass(driver, what: str) -> None:
    """Poke the member seat's loop and wait for a pass that began after now."""
    started, _ = alert_sweep_passes(driver) or (0, 0)
    driver.call_command(ALERT_SWEEP_WAKE)
    await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what=what)


@pytest.fixture
def admin_tui_seat(request, nest_instance):
    """A second seat: ``tui`` signed in as the nest's admin, on its admin shell.

    A fresh launch with its own state root (``_build_app_config``), torn down
    with the test, so it never shares a store with the member's seat."""
    from actions import ActionLayer
    from conftest import _build_app_config, _login_admin_as
    from drivers import create_driver

    driver = create_driver("tui")
    driver.launch(_build_app_config("tui", nest_instance, request))
    try:
        seat = ActionLayer(driver)
        _login_admin_as(
            seat, request, nest_instance,
            spa_url_fixture="spa_url", fixture_name="admin_tui_seat",
        )
        yield seat
    finally:
        try:
            driver.screenshot("teardown-eviction-admin-seat")
        except Exception:
            pass
        driver.teardown()


@pytest.mark.feature("critical-alerts")
def test_an_evicted_member_is_warned_with_both_dates_and_a_cancel_clears_it(
    app, nest_instance, request, admin_tui_seat
):
    """The admin evicts a member from the Users page; the member's app, after
    its next sweep, shows the eviction warning with the date access ends and
    the deletion date; the admin calls it off; a later sweep clears it."""
    from conftest import _login_app_as, _make_user

    member = _make_user(nest_instance)
    actor_hex = member["actor_id_hex"]

    # ── The member signs in; the login-dispatched pass finds nothing, since
    #    nothing exists yet to find (true by construction, not by timing). ──
    before_login, _ = alert_sweep_passes(app.driver) or (0, 0)
    _login_app_as(app, request, nest_instance, member)
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    await_sweep_pass_after(
        app.driver, before_login, budget_s=ALERT_SWEEP_PASS_S, what="the session-start sweep"
    )
    assert _HEAD not in _alert_text(app.driver), "precondition: no eviction stands yet"

    # ── The admin evicts the member from the Users page. ──
    admin = admin_tui_seat.admin
    def member_listed() -> bool:
        admin.navigate_users()
        return admin.has_user_row(actor_hex)

    wait_until(
        member_listed,
        30.0,
        interval=1.0,
        diagnose=lambda: f"the member never appeared on the Users page: {admin.user_actor_ids()!r}",
    )
    row = admin.user_row_index(actor_hex)
    assert admin.user_evict_available(row), "precondition: the member is not under eviction"
    admin.evict_user(row)
    assert admin.user_eviction_active(row), (
        "the evict click did not start the eviction; error surface: "
        f"{admin_tui_seat.error_text()!r}"
    )

    # ── The member's next sweep raises the warning, on a page that is not the
    #    one any of this happened on. ──
    _wake_and_await_pass(app.driver, "the re-sweep after the eviction")
    text = _poll_banner(app.driver, present=True)
    assert _HEAD in text, (
        "an eviction in its warning phase must reach the member's every-page "
        f"banner; banner reads {text!r}, error surface: {app.error_text()!r}"
    )
    dates = _UTC_MINUTE.findall(text)
    assert len(dates) == 2, f"the line names the date access ends and the deletion date: {text!r}"
    suspend_at, delete_at = (datetime.strptime(d, "%Y-%m-%d %H:%M") for d in dates)
    assert delete_at - suspend_at == timedelta(days=14), (
        "deletion follows the end of access by the hard-coded suspension "
        f"window (node_policy, 14 days); got {dates!r}"
    )
    assert "Export My Data" in text, f"the line names the remedy: {text!r}"

    # ── The admin calls the eviction off; a later sweep clears the banner. ──
    admin.navigate_users()
    row = admin.user_row_index(actor_hex)
    admin.cancel_user_eviction(row)
    assert not admin.user_eviction_active(row), (
        "the cancel click did not call the eviction off; error surface: "
        f"{admin_tui_seat.error_text()!r}"
    )
    _wake_and_await_pass(app.driver, "the re-sweep after the cancel")
    text = _poll_banner(app.driver, present=False)
    assert _HEAD not in text, (
        f"a called-off eviction must take its banner with it; banner reads {text!r}"
    )
