"""tier_3 API: a succession the nest committed but never answered.

Goal doc: ``docs/goal/behavior/identity-succession.md`` § Enforcement on the
home nest (the one atomic transaction, committed before the reply is encoded)
and § Implementation status today (*a lost submit reply no longer destroys the
account*).

The convention-5 companion to the lost-reply journeys in
``tests/test_identity_succession_ceremony.py``. Those drive the app; this pins
the nest half with no app in the picture, so a red there can be split into
"the nest did not stage the state" and "the app mishandled it" in seconds:

* the drop hook (``helpers/rpc_hold.drop_rpc_reply``) runs the REAL submit
  handler and then closes the connection instead of replying, so the caller
  meets a transport failure — not an RPC refusal;
* and the succession is committed anyway: the anonymous lookup serves back
  the very statement that was submitted.

That pair is the whole premise the app-side reconcile rests on. A hook that
faked the commit, or refused before the handler ran, would fail the second
assertion here rather than surfacing as an inexplicable UI outcome later.

Latency-independent (convention 14): every call is a request with a reply (or
a closed link), every assertion is on returned state; nothing sleeps.
``standalone_only``: the hook exists only in a nest compiled with
``test-hooks`` (convention 15), which no docker or live artifact is.
"""
from __future__ import annotations

import secrets

import pytest

from clients._ws_rpc_core import RpcCallError, WsLinkDied
from common.auth import create_actor_and_register
from helpers.rpc_hold import drop_rpc_reply, release_rpc_hold, rpc_hold_status
from helpers.succession import (
    mint_statement,
    register_recovery_kit,
    registration_chain,
    submit_statement,
    succession_statements,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]

_SUBMIT_KIND = "fauna.recovery.succession.submit"


@pytest.mark.feature("take-your-account-back")
def test_a_dropped_submit_reply_fails_at_transport_while_the_succession_lands(
    nest_instance,
):
    """arm the drop → submit → the call dies on the link, not with an RPC
    error → the anonymous lookup serves back the exact statement submitted."""
    nest = nest_instance
    # Dedicated: the succession re-points this account and revokes every
    # session of it, which must never happen to a user another test shares.
    user = create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )
    seed_hex = bytes(user["signing_key"]).hex()
    kit_secret = register_recovery_kit(
        nest["url"], actor_id_hex=user["actor_id_hex"], identity_seed_hex=seed_hex
    )
    _successor_id, statement = mint_statement(
        old_actor_id_hex=user["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=secrets.token_bytes(32).hex(),
        registrations=registration_chain(nest["url"], user["actor_id_hex"]),
        old_seed_hex=seed_hex,
    )
    assert succession_statements(nest["url"], user["actor_id_hex"]) == [], (
        "precondition: the identity must not be succeeded before the submit"
    )

    dropped_before = rpc_hold_status(nest["port"], _SUBMIT_KIND)["dropped"]
    drop_rpc_reply(nest["port"], _SUBMIT_KIND)
    try:
        with pytest.raises(WsLinkDied) as died:
            submit_statement(nest["url"], statement)
    except RpcCallError as e:  # pragma: no cover - the failure message
        pytest.fail(
            "the submit came back as an RPC error — the caller must meet a "
            f"TRANSPORT failure over a lost reply, never a refusal: {e!r}"
        )
    finally:
        release_rpc_hold(nest["port"], _SUBMIT_KIND)

    assert rpc_hold_status(nest["port"], _SUBMIT_KIND)["dropped"] == dropped_before + 1, (
        f"the nest must record exactly this reply as dropped; the link died "
        f"with {died.value!r}"
    )
    assert succession_statements(nest["url"], user["actor_id_hex"]) == [statement], (
        "the submit handler must have run and COMMITTED before its reply was "
        "dropped — the lookup has to serve back the very statement submitted, "
        "or the hook staged 'never ran' rather than 'landed, unanswered'"
    )
