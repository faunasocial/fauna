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
"""
from __future__ import annotations

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import app_name, skip_unbuilt
from helpers.succession_ceremony import SECRET_HEX_LEN, wait_for_successor_actor
from i18n.strings import S

pytestmark = pytest.mark.tier_3


def _require_locked_surface(driver) -> None:
    """Convention 7: tui is the lead app (built first); the other six route
    ``FfiError::AccountLocked`` / the wasm classifier to this surface in their
    trickle-down leg."""
    if app_name(driver) != "tui":
        skip_unbuilt(
            driver,
            surface="locked launch surface",
            detail="tui leads; this app paints launch-account-locked-notice in its trickle-down leg",
            tracked="docs/goal/behavior/devices.md § Implementation status today (gap 4)",
        )


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
    _require_locked_surface(driver)
    nest_url = nest_instance["url"]
    old_actor = user["actor_id_hex"]

    # ── 0. The kit, minted while the owner can still sign in ──────────────
    # The credential the thief does not hold: without one minted earlier, a
    # locked account has no remedy at all.
    app.settings.open_recovery_kit_or_skip()
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
