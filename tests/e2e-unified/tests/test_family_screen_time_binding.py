"""family-safety outcome 67, the binding half — a guardian's screen-time edit
binds the child's app without a restart.

`docs/goal/behavior/family-client-enforcement.md` § Implementation status today:
"Both legs re-read the policy on the ward's own Family-page visit, so a
guardian's edit binds without a restart, and re-evaluate the clock on a
one-minute tick". The child's app stays up throughout: the guardian's window
lands while it runs, the child opens their Family page, and the lock is up on
the next page — then the guardian moves the window to include now, and the next
Family visit lifts it, again with no relaunch.

The guardian's edits arrive over the wire (`set_ward_policy` — the same
`fauna.family.policy.update` the editor sends; the editor gesture itself is
`test_family_screen_time_window_lock`'s subject), because the guardian's own app
would be this same app instance and the child's launch must stay up. What is
under test is the CHILD's app picking the edit up.

**Windows relative to now with hours of margin** (convention 14), as in
`test_family_screen_time_window_lock`: the locking window opens 3h from now and
closes 4h from now; the allowing one spans now−2h..now+2h.

The billing half of the outcome — only time in front and unlocked bills, and a
failed report's minutes are carried to the next one — is witnessed by
`test_family.py::test_family_screen_time_bills_only_time_in_front_and_a_failed_report_is_not_lost`,
over the `screen_time_heartbeat` agent command's `focused: false` face, with the
rules themselves pinned at tier_1 (`screen_lock::tests`,
`fauna_core::screen_time::tests`).
"""

import time

import pytest

from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.family_wire import set_ward_policy
from helpers.waiting import wait_until
from tests.test_family import _shared_family, family_pair  # noqa: F401

pytestmark = pytest.mark.tier_3


def _minutes_from_midnight_now() -> int:
    now = time.localtime()
    return now.tm_hour * 60 + now.tm_min


@pytest.mark.feature("family-safety")
def test_family_a_guardians_screen_time_edit_binds_the_running_app_without_a_restart(
    admin_app, request, nest_instance, family_pair
):
    from conftest import _login_app_as

    app = admin_app
    guardian_identity, ward_identity, guardian_handle, _ward_handle = family_pair
    now_minutes = _minutes_from_midnight_now()
    locked = ((now_minutes + 180) % 1440, (now_minutes + 240) % 1440)
    allowed = ((now_minutes - 120) % 1440, (now_minutes + 120) % 1440)

    # ── The child signs in, unlocked. ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    app.driver.navigate_to("feed")
    assert not app.family.screen_lock_visible(), (
        f"precondition: no window set, so no lock: {app.driver.diagnose('screen-time-lock')}"
    )

    # ── The guardian's window lands while the child's app runs. ──
    set_ward_policy(
        nest_instance,
        guardian_identity,
        ward_identity,
        screen_time={"window_start": locked[0], "window_end": locked[1]},
    )

    def _locked() -> bool:
        app.driver.navigate_to("family")
        app.driver.navigate_to("feed")
        return app.family.screen_lock_visible()

    wait_until(
        _locked,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        interval=1.0,
        diagnose=lambda: (
            "the guardian's window must lock the running app after the child's Family "
            f"visit, with no restart; {app.driver.diagnose('screen-time-lock')} "
            f"error: {app.error_text()!r}"
        ),
    )
    assert guardian_handle in app.family.screen_lock_message(), (
        f"the lock must name the guardian: {app.family.screen_lock_message()!r}"
    )

    # ── The guardian widens the window to include now; the lock lifts. ──
    set_ward_policy(
        nest_instance,
        guardian_identity,
        ward_identity,
        screen_time={"window_start": allowed[0], "window_end": allowed[1]},
    )

    def _unlocked() -> bool:
        app.driver.navigate_to("family")
        app.driver.navigate_to("feed")
        return not app.family.screen_lock_visible()

    wait_until(
        _unlocked,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        interval=1.0,
        diagnose=lambda: (
            "the guardian's widened window must lift the lock on the running app with "
            f"no restart; {app.driver.diagnose('screen-time-lock')} "
            f"error: {app.error_text()!r}"
        ),
    )
