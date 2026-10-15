"""tier_3: a sibling device whose replica is stale across another device's
mail-key rotation never re-seals the MDA snapshot under the custody it has not
walked — IMAP keeps working after it reconnects.

The witness for ``docs/goal/behavior/mail-credentials.md`` § Hard revoke →
*Published before the nest moves*, rule **(4)** (a stale sibling re-seals only
under a walked custody), under the recoverability invariant
(``docs/goal/architecture/nest/common.md`` § Client-state recoverability).

**The window it stands open.** Device B listed the fleet scope before device
A's rotation and has not walked since — a laptop asleep through the rotation.
Its reconnect runs the connect-time refresh, which reads the custody B holds:
the pre-rotation MSEK, no sentinel. Re-sealing under it would seal the
snapshot under a key no credential's blob names, and every MUA's AUTH would
fail. Rule (4): the refresh reads back the nest's recipient pubkey first, and
a generation its custody has never held makes it walk the fleet scope
(``walk_fleet``) before deciding, and re-seal under the walked custody.

  A: mail on (PLAIN) → one pump pass ships the enable rows → B (a second tui
  seat of the same account) launches under the **fleet-walk hold** and lists
  the custody → its own launch refresh re-seals → A rotates, then closes →
  **the nest bounces and B reconnects** (a real WS reconnect in B's running
  process, which runs the connect-time refresh) → B's refresh walks and
  re-seals (the snapshot row changes) → IMAPS AUTH with the surviving
  credential succeeds.

**The walk lands wherever the engine role is.** The bounce reconnects B's
sync agent too, and the agent — the engine role's preferred holder — usually
takes the role back from B's app as it does. B's app is then a non-holder,
and its awaited walk reaches the holder (``docs/goal/architecture/account-runtime.md``
§ Multi-instance concurrency → *A non-holder's awaited fleet walk reaches the
holder*): the agent walks on its behalf over ``WalkFleetNow`` and B's app reads
the walked custody from the store the two share, logging that the holder
walked for it. When B's app still holds the role, it walks itself. Either
way the re-seal is required on every run; a refresh that declines to walk
("no walk can be taken now") is red here.

**Held, so B is stale every run.** Without the hold, A's rotation nudges B's
live session and B's walk lands within moments, so B would almost never be
stale when it reconnects. Under the hold (``helpers/account_runtime_holds.py``)
no unprompted walk of B's — a nudge's, a pass's — lands after its launch
listing; only the refresh's own awaited ``walk_fleet`` does. B's agent is
held too: the e2e spawner starts it as a direct child of B's tui process
(never through ``systemctl --user``), so it inherits the hold variable — and
its ``WalkFleetNow`` walk is the awaited leg the hold spares. On a tree without
rule (4) B re-seals under the pre-rotation key and AUTH fails, every run. The
tier_1 witnesses are ``libs/fauna-client-mail-settings/tests/rotation_resume.rs``'s
three stale-sibling cases.

**Non-vacuous.** B's launch-time refresh is awaited (its re-seal observed)
before A rotates, so a re-seal after the bounce is B's reconnect's. A is
closed before the bounce: its custody is current, so its own reconnect would
re-seal correctly and could hide B's. And the bounce is the only thing that
reconnects B, so on a tree whose reconnect does not run the refresh no
re-seal is ever observed — red at the wait, as it is red at the AUTH on a
tree without rule (4), and red at the decline on a tree whose non-holder walk
does not reach the holder.

tier_3: every binary real, two real tui processes on one account, the real
IMAPS wire.
"""

from __future__ import annotations

import pytest

from common.nest import restart_nest
from helpers.account_runtime_holds import WALK_HOLD_ENV
from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.fleet import SiblingSeats, poked_pass
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from helpers.mail_reseal import imap_auth_and_select, snapshot_blob, wait_for_reseal
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
]

_MUA_PASSWORD = "rotate-plain-pw-2"

_MAIL_LOG_MARKERS = (
    "mail",
    "reconnect",
    "walk",
    "engine role",
    "ws_adapter",
    "ws_device_handshake",
)

#: The line `MailSettingsMachine::refresh_epoch_schedule` logs when the nest's
#: recipient key names a generation the custody never held and no walk can be
#: taken now, so it moves nothing (`libs/fauna-client-mail-settings/src/machine.rs`).
_REFRESH_DECLINED = "names a generation this replica has not walked"

#: The line `AccountStoreHandle::walk_fleet` logs when this process is not the
#: engine-role holder and the holder walked for it
#: (`libs/fauna-account-plane/src/account_driver/handle.rs`).
_HOLDER_WALKED = "the engine holder walked the fleet scope for this non-holder"


def _declines(seat) -> int:
    return seat.driver.app_stderr_text().count(_REFRESH_DECLINED)


def _holder_walks(seat) -> int:
    return seat.driver.app_stderr_text().count(_HOLDER_WALKED)


def _mail_log(seat) -> str:
    """The seat's own log lines about its connections and its mail refresh —
    what decides whether a reconnect ran the refresh and what the refresh did."""
    lines = [
        line
        for line in seat.driver.app_stderr_text().splitlines()
        if any(marker in line for marker in _MAIL_LOG_MARKERS)
    ]
    return "device B's connection and mail log:\n" + "\n".join(lines[-60:])


@pytest.mark.feature("turn-on-mail")
def test_a_stale_sibling_reconnecting_after_a_rotation_keeps_imap_working(
    app, dedicated_mail_nest, request
):
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    # ── 1. A: mail on (PLAIN), an address routed to this account, the gates
    # rebound; then one pump pass ships the enable rows, so B's launch
    # listing holds the custody.
    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    recipient = alias_admin_to_address(nest, domain)
    app.mail_settings.navigate()
    app.mail_settings.enable_mail_plain(_MUA_PASSWORD)
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        "enabling mail must flip the page to enabled; "
        f"status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    handle.rebind_after_enable()
    imap_auth_and_select(handle, recipient, _MUA_PASSWORD, "before the rotation")
    poked_pass(app.driver, what="mail was enabled")

    seats = SiblingSeats(request, nest)
    a_closed = False
    try:
        # ── 2. B: a second seat of the same account, under the walk hold. Its
        # launch listing is never held; every unprompted walk after it is.
        before_b = snapshot_blob(nest)
        device_b = seats.launch(user=nest["admin"], environment={WALK_HOLD_ENV: "1"})
        device_b.mail_settings.navigate()
        assert device_b.mail_settings.wait_for_enabled_status(timeout=ORCHESTRATION_STEP_S), (
            "device B's launch listing must hold the custody A's pump pass "
            f"shipped; status={device_b.mail_settings.status_text()!r}"
        )
        wait_for_reseal(nest, before_b, "device B's launch refresh")

        # ── 3. A rotates. B's replica stays at what it listed.
        app.mail_settings.rotate_keys()
        assert app.mail_settings.wait_for_rotation_to_finish(ORCHESTRATION_STEP_S), (
            "the rotation never finished: the rotate form is still open with "
            f"progress {app.driver.get_text('mail-rotate-keys-progress-indicator')!r}"
        )
        rotated = snapshot_blob(nest)

        # ── 4. A closes (its rotation is acknowledged by the nest before the
        # form closes — rule (3) — so nothing of it is left to publish), then
        # the nest bounces: B's running process reconnects, and its
        # connect-time refresh runs over the stale custody.
        declines_before = _declines(device_b)
        app.driver.teardown()
        a_closed = True
        restart_nest(nest, graceful=True)
        holder_walks_before = _holder_walks(device_b)
        decision = wait_until(
            lambda: (
                "re-sealed (the snapshot row changed)"
                if snapshot_blob(nest) != rotated
                else "declined"
                if _declines(device_b) > declines_before
                else None
            ),
            ORCHESTRATION_STEP_S,
            diagnose=lambda: "device B's reconnect never re-sealed the snapshot "
            "(no re-seal, no logged decline)\n" + _mail_log(device_b),
        )
        assert decision != "declined", (
            "device B's refresh moved nothing — its awaited walk was refused "
            "rather than walked by the engine-role holder\n" + _mail_log(device_b)
        )
        walked_by = (
            "the sync agent, for B's app"
            if _holder_walks(device_b) > holder_walks_before
            else "B's app, as the holder"
        )
        print(f"device B's reconnect refresh: {decision}; the walk was taken by {walked_by}")
        imap_auth_and_select(
            handle, recipient, _MUA_PASSWORD, "after the stale sibling reconnected"
        )
    finally:
        seats.teardown()
        if a_closed:
            # Reopen A, so the frame probe and the next test meet a live app.
            app.driver.hard_reload()
