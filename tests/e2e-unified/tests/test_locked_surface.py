"""Lock → the locked surface → the stolen-identity ceremony → signed in as the
successor, through the app UI, with the nest's own answers as the outside
witness.

Owner: ``docs/goal/ui/sessions.md`` (§ Layout & flow → *The locked surface
(leg 2)*, § Done definition — the second tier_3 journey) over
``docs/goal/behavior/devices.md`` § The locked state and § The two panic
buttons, point 5: the ceremony "must be reachable from a locked-out app and must
ride an anonymous connection".

Arrangement: a DEDICATED non-admin actor (``succeedable_app``). Dedicated
because both acts are irreversible for the identity — the lock freezes it for
24 hours and the ceremony retires it. Non-admin because admins are exempt from
the verify lock (``login.md`` § Silent Challenge, ruled 2026-10-01): a locked
admin still signs in, and the surface under test is never reached. The journey
asserts that precondition from outside rather than assuming it.

Every act goes through the UI (convention 8): the kit is created in Settings,
the lock is pressed on the Sessions page, the ceremony is run from the locked
surface. Every verdict that could be UI-side or nest-side is read back over the
wire with the actor's own key (convention 5): ``fauna.auth.verify`` refusing
the locked identity with ``fauna.auth.account_locked``, then minting for nobody
but the successor.

The second journey witnesses the lock's other edge (``devices.md`` § The locked
state; ``sessions`` catalog outcome 24): a locked app stops retrying, and signs
back in by itself once the lock ends. The lock is a hard-coded 24 hours, so the
nest's test hook (``account_lock_test_hook.rs``) arranges a NEAR unlock time —
armed before the relaunch and written at the gate by the relaunched app's own
first sign-in, so the refusal the app meets carries it — and counts every
sign-in it refuses as locked, which is the outside witness for "stops
retrying". No app hook: the app's own one scheduled refresh comes due inside
the run.
"""
from __future__ import annotations

import json
import urllib.request

import pytest

from clients._ws_rpc_core import RpcCallError
from common.launch_harness import reached_authenticated_app
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import require_surface
from helpers.relaunch import pin_store_across_relaunch
from helpers.succession_ceremony import SECRET_HEX_LEN, wait_for_successor_actor
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3


def _verify_refusal(nest_url: str, user: dict) -> str | None:
    """What ``fauna.auth.verify`` answers this identity right now: the refusal's
    wire code, or ``None`` when it mints a bearer. Every app bearer is minted
    here, so this is the lock as every launch path meets it."""
    client = WsRpcAdminClient(
        nest_url, bytes.fromhex(user["actor_id_hex"]), bytes(user["signing_key"])
    )
    try:
        with client:
            client.own_token_id
    except RpcCallError as refused:
        return refused.code
    return None


@pytest.mark.feature("sessions")
def test_lock_then_the_locked_surface_then_the_kit_signs_you_in_as_the_successor(
    succeedable_app, nest_instance
):
    app, user = succeedable_app
    driver = app.driver
    nest_url = nest_instance["url"]
    old_actor = user["actor_id_hex"]

    # ── 0. The kit, minted while the owner can still sign in ──────────────
    # The credential the thief does not hold: without one minted earlier, a
    # locked account has no remedy at all.
    app.settings.open_recovery_kit()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error surface: {app.error_text()!r}"
    )
    assert _verify_refusal(nest_url, user) is None, (
        "precondition: before the lock the identity signs in"
    )

    # ── 1. Lock, from the Sessions page (leg 1's control) ─────────────────
    app.sessions.navigate()
    app.sessions.lock()

    # ── 2. The locked surface ─────────────────────────────────────────────
    notice = app.sessions.wait_for_locked_notice()
    assert S.onboarding.launch.account_locked in notice, notice
    assert S.onboarding.launch.account_locked_not_yours in notice, (
        "the notice must say that a lock the owner did not set means somebody "
        f"holds the secret key — without it the surface reads as 'wait'; got {notice!r}"
    )
    # The unlock time: the sentence around the placeholder, rendered with a
    # real value (the app formats it for its locale, so only its presence and
    # shape are asserted — a lock is 24 hours, so the year is this one or next).
    before, after = S.onboarding.launch.account_locked_until(time="\x00").split("\x00")
    assert before in notice and after in notice, (
        f"the notice must carry the unlock-time sentence; got {notice!r}"
    )
    shown = notice.split(before, 1)[1].split(after, 1)[0] if after else notice.split(before, 1)[1]
    assert any(ch.isdigit() for ch in shown) and "{" not in shown, (
        f"the unlock time must be a rendered time, got {shown!r} in {notice!r}"
    )
    # One action only: nothing clears the lock before its time, and "use a
    # different nest" would misstate the problem.
    assert driver.is_absent("launch-retry-button"), "a locked account offers no retry"
    assert driver.is_absent("launch-fallthrough-button"), (
        "a locked account offers no fallthrough"
    )
    # The outside witness: the nest refuses this identity at verify, by name.
    # Were the actor an admin this would be None and the surface above could
    # not have painted — the two agree or the journey is wrong about its actor.
    assert _verify_refusal(nest_url, user) == "fauna.auth.account_locked", (
        "the nest must refuse the locked identity's sign-in as locked"
    )

    # ── 3. The stolen-identity ceremony, with no session ──────────────────
    app.sessions.open_stolen_entry()
    assert not app.is_enabled("identity-stolen-button"), (
        "the type-to-confirm gate starts unarmed, so one stray press cannot "
        "re-point an account"
    )
    app.sessions.succeed_from_locked(held)

    # ── 4. Signed in as the successor ─────────────────────────────────────
    new_actor = wait_for_successor_actor(app, old_actor)
    assert new_actor != old_actor
    assert len(new_actor) == SECRET_HEX_LEN, new_actor
    # From outside: the old key no longer describes a locked account — it
    # describes a retired one — so the lock did not survive the ceremony
    # (`identity-succession.md` § Implementation status today, *The
    # transaction*: the successor never inherits `locked_until`). The
    # successor's own live session, asserted by the wait above, is the other
    # half: a successor born locked could not have signed in.
    assert _verify_refusal(nest_url, user) == "fauna.auth.superseded", (
        "the retired identity must be refused as superseded, not as locked"
    )


#: How long the arranged lock lasts, counted from the moment the relaunched
#: app's first sign-in reaches the nest's gate (not from the relaunch, whose
#: latency is unknowable). It must cover the refusal reaching the app and the
#: locked notice painting — so a slow box lengthens what happens INSIDE the
#: window, never moves the window itself — and it is long against any reconnect
#: back-off, so an app that kept retrying would be refused again inside it.
LOCK_WINDOW_S = 45

#: The app's refresh lands a few seconds past ``locked_until``
#: (``LOCK_LAPSE_GRACE_SECS``); the rest is generous slack for the sign-in round
#: trip on a loaded box (convention 14 — a green run returns on its first poll).
LAPSE_SIGN_IN_BUDGET_S = LOCK_WINDOW_S + 120


def _lock_at_next_sign_in(port: int, actor_hex: str, in_secs: int) -> None:
    """Arm a lock of ``actor_hex`` that its next ``fauna.auth.verify`` — the
    silent challenge's mint, the sign-in every app launch makes — writes at the
    nest's gate as ``now + in_secs`` (test hook). The window opens at the gate,
    never while the app is still relaunching: parking the verify and writing the
    lock from here instead raced the verify's own 5-second client deadline, and
    a relaunch slower to hand back control than that timed the app out into its
    offline surface before the lock existed."""
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/api/v1/test/account_lock/{actor_hex}/at_next_verify"
        f"?in_secs={in_secs}",
        data=b"",
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        assert json.loads(resp.read())["ok"] is True


def _locked_refusals(port: int, actor_hex: str) -> int:
    """Sign-ins of ``actor_hex`` the nest has refused as locked, so far."""
    with urllib.request.urlopen(
        f"http://127.0.0.1:{port}/api/v1/test/account_lock/{actor_hex}/refusals"
    ) as resp:
        return json.loads(resp.read())["refused"]


@pytest.mark.feature("sessions")
def test_a_locked_app_stops_retrying_and_signs_back_in_when_the_lock_ends(
    succeedable_app, nest_instance
):
    app, user = succeedable_app
    driver = app.driver
    # The relaunch must come back as this identity, signing itself in from its
    # own store: the store has to survive (the default relaunch hands the app a
    # fresh one), and the injected identity has to have reached it.
    pin_store_across_relaunch(driver)
    require_surface(driver, "relaunch-injected-identity")
    port = nest_instance["port"]
    actor = user["actor_id_hex"]

    # ── 1. Relaunch into a near lock ──────────────────────────────────────
    # Armed BEFORE the relaunch and written by the nest when the relaunched
    # app's first sign-in reaches the gate — so the refusal it meets carries an
    # unlock time LOCK_WINDOW_S away from that moment, whatever the relaunch
    # cost.
    _lock_at_next_sign_in(port, actor, LOCK_WINDOW_S)
    assert driver.recover(), "the app did not come back up after the relaunch"

    # ── 2. Locked, and refused at the gate ────────────────────────────────
    notice = app.sessions.wait_for_locked_notice()
    assert S.onboarding.launch.account_locked in notice, notice
    refused_at_lock = _locked_refusals(port, actor)
    assert refused_at_lock >= 1, (
        "the locked surface painted, but the nest refused no sign-in as locked — "
        "the counter is not watching the gate the app signs in through"
    )

    # ── 3. Signed back in by itself, with no retry in between ─────────────
    # No gesture, no relaunch: the app's one scheduled refresh is the only way
    # out of the locked state before the lock ends.
    reached_authenticated_app(driver, timeout=LAPSE_SIGN_IN_BUDGET_S)
    wait_until(
        lambda: driver.is_absent("launch-account-locked-notice"),
        30.0,
        diagnose=lambda: "signed in, but the locked notice is still up",
    )
    retried = _locked_refusals(port, actor) - refused_at_lock
    assert retried == 0, (
        f"a locked app must stop retrying until the lock ends, but the nest "
        f"refused {retried} more sign-in(s) as locked between the locked surface "
        f"and the sign-in"
    )
    # From outside: the lock has ended for every sign-in path, not just the app's.
    assert _verify_refusal(nest_instance["url"], user) is None, (
        "the lock has ended, so the identity must sign in again"
    )
