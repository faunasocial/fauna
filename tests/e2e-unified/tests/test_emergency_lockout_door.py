"""The signed-out lock door — lock an account from a device that holds nothing,
through the app UI, with the nest's own answer as the outside witness.

Owner: ``docs/goal/ui/sessions.md`` (§ Layout & flow → *The signed-out door
(leg 3)*, § Done definition) over ``docs/goal/behavior/devices.md`` § The
signed-out door: the owner wants an account locked from a device they do not
want to sign in on, so the door takes the secret key and the handle, signs
``fauna.account.lockout`` over an anonymous connection, and **persists nothing**.

Arrangement: a DEDICATED non-admin actor (``succeedable_app``) — the lock
freezes it for 24 hours, and admins are exempt from the verify lock
(``login.md`` § Silent Challenge), which would leave the outside witness silent.
The app is then factory-reset: from there it is a fresh tui holding nothing but
what the test types, which is the precondition the door exists for.

Every act goes through the UI (convention 8). Every verdict that could be
UI-side or nest-side is read back over the wire with the actor's own key
(convention 5): ``fauna.auth.verify`` refusing the identity with
``fauna.auth.account_locked``. The door's whole contract — nothing kept — is read
off the app's credential store at rest, before and after.

tier_3: needs a real ``fauna-nest`` binary. tui leads; the other six join in
their trickle-down legs.
"""
from __future__ import annotations

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import skip_unbuilt
from helpers.recovery_restore import point_the_restore_at, qualified_account
from helpers.registry_store import read_store_map
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: A landed lock or a refusal rides one anonymous round trip; a generous ceiling
#: paid only on red (convention 14).
DOOR_OUTCOME_S = 45.0


def _verify_refusal(nest_url: str, user: dict) -> str | None:
    """What ``fauna.auth.verify`` answers this identity right now: the refusal's
    wire code, or ``None`` when it mints a bearer."""
    client = WsRpcAdminClient(
        nest_url, bytes.fromhex(user["actor_id_hex"]), bytes(user["signing_key"])
    )
    try:
        with client:
            client.own_token_id
    except RpcCallError as refused:
        return refused.code
    return None


def _open_the_door_on_a_fresh_app(app, nest_instance) -> None:
    """Factory-reset to a device that holds nothing and open the lock door."""
    app.driver.reset()
    app.onboarding.navigate_to_status()
    if not app.is_visible("emergency-lockout-entry-button"):
        skip_unbuilt(
            app.driver,
            surface="emergency-lockout-entry-button",
            detail="ui/sessions.md § The signed-out door; tui leads and the other six follow in batched trickle-down",
            tracked="docs/goal/ui/sessions.md § Implementation status today",
        )
    point_the_restore_at(app, nest_instance)
    app.onboarding.open_emergency_lockout_entry()


@pytest.mark.feature("sessions")
def test_the_door_locks_an_account_from_a_device_that_holds_nothing(
    succeedable_app, nest_instance
):
    app, user = succeedable_app
    nest_url = nest_instance["url"]
    account = qualified_account(app, nest_instance)
    secret_hex = bytes(user["signing_key"]).hex()
    assert _verify_refusal(nest_url, user) is None, (
        "precondition: before the lock the identity signs in"
    )

    _open_the_door_on_a_fresh_app(app, nest_instance)
    store_before = read_store_map(app.driver)
    assert not any(k.endswith("/secret") for k in store_before), (
        f"precondition: a factory-reset device holds no identity; store keys {sorted(store_before)}"
    )

    # The REQUIRED warning is always on the page, never behind the confirm, and
    # the button starts unarmed so a stray press cannot lock an account.
    assert app.is_visible("sessions-lockout-warning")
    assert not app.is_enabled("sessions-lockout-button"), (
        "the type-to-confirm gate starts unarmed"
    )

    app.onboarding.lock_from_the_door(secret_hex, account=account)

    # A landed lock returns to identity_choice, where the confirmation speaks.
    wait_until(
        lambda: app.is_visible("emergency-lockout-entry-button")
        and app.has_error(),
        DOOR_OUTCOME_S,
        diagnose=lambda: (
            "the lock did not land back on identity_choice with its confirmation; "
            f"error surface says {app.error_text()!r}, still on the door="
            f"{app.is_visible('sessions-lockout-button')}"
        ),
    )
    shown = app.error_text() or ""
    before, after = S.onboarding.emergency_lockout.locked(until="\x00").split("\x00")
    assert before in shown and after in shown, (
        f"the confirmation must carry the unlock-time sentence; got {shown!r}"
    )
    unlock = shown.split(before, 1)[1].split(after, 1)[0] if after else shown.split(before, 1)[1]
    assert any(ch.isdigit() for ch in unlock) and "{" not in unlock, (
        f"the unlock time must be a rendered time, got {unlock!r} in {shown!r}"
    )

    # The outside witness: the nest refuses this identity at verify, by name.
    assert _verify_refusal(nest_url, user) == "fauna.auth.account_locked", (
        "the nest must refuse the locked identity's sign-in as locked"
    )

    # The door's whole contract: nothing was kept. The credential store at rest
    # is byte-for-byte what it was before the secret was pasted.
    assert read_store_map(app.driver) == store_before, (
        "the signed-out door persisted something; the credential store changed"
    )


def test_a_recovery_phrase_is_not_a_secret_key_and_nothing_is_locked(
    succeedable_app, nest_instance
):
    app, user = succeedable_app
    nest_url = nest_instance["url"]
    account = qualified_account(app, nest_instance)

    _open_the_door_on_a_fresh_app(app, nest_instance)
    # A recovery kit is the deliberate twin of the identity secret on another
    # host; pasting one here is the mistake worth naming.
    kit = (
        "fauna://recovery?secret="
        + "7c" * 32
        + "&actor="
        + user["actor_id_hex"]
    )
    app.onboarding.lock_from_the_door(kit, account=account)

    wait_until(
        lambda: app.has_error(),
        DOOR_OUTCOME_S,
        diagnose=lambda: "a recovery phrase must refuse on error-message, not sit silent",
    )
    assert app.is_visible("sessions-lockout-button"), (
        "the user stays on the door to paste the right thing"
    )
    assert S.onboarding.emergency_lockout.invalid_secret in (app.error_text() or "")
    assert _verify_refusal(nest_url, user) is None, (
        "a refused paste must lock nothing"
    )
