"""The first succession wins, and a suspension survives it — tier_3 API E2E.

Witnesses `docs/features/take-your-account-back.md` outcome **51**: *"The first
succession wins: a later holder of the old kit cannot re-point the account
away, and an admin's suspension carries to the successor."* The rulings are
`docs/goal/behavior/identity-succession.md` § Enforcement on the home nest —
`actor_successions.old_actor_id` is the table's primary key, read inside the
transaction for the typed `fauna.recovery.already_succeeded` refusal
(`bins/fauna-nest/src/db/successions.rs::record_succession`), and the
successor's `users` row inherits `suspended` because a moderation verdict is
about the account, not the key.

Observed from outside, over the production kinds and no app: the ceremony is
the real one (`helpers/succession.py` + the Rust `recovery_fixture`), so every
statement below verifies against the chain the identity registered. The second
holder's statement is genuine too — signed with the same kit over the same
chain — which is what makes its refusal the first-wins rule and not a bad
signature.

Every actor is fresh and every effect is scoped to it, so the session nest is
safe. Latency-independent (convention 14): requests with replies, nothing
sleeps or polls.
"""

from __future__ import annotations

import secrets

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register, mint_token_via_handshake
from helpers.admin_wire import admin_client
from helpers.succession import (
    mint_statement,
    register_recovery_kit,
    registration_chain,
    submit_statement,
    succeed_identity,
    succession_statements,
)

pytestmark = pytest.mark.tier_3

#: The typed refusal `succession_submit_handler` maps `AlreadySucceeded` to.
ALREADY_SUCCEEDED = "fauna.recovery.already_succeeded"


def _fresh_actor(nest) -> dict:
    return create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )


def _admin_user_row(nest, actor_id_hex: str) -> dict | None:
    want = bytes.fromhex(actor_id_hex)
    with admin_client(nest) as admin:
        users = admin.users_list_all()
    return next((u for u in users if bytes(u["actor_id"]) == want), None)


@pytest.mark.feature("take-your-account-back")
def test_a_later_holder_of_the_old_kit_cannot_re_point_the_account_away(nest_instance):
    """Two holders of one kit: the first succession lands, the second — an
    equally genuine statement over the same chain naming a different key — is
    refused as already succeeded, and the account stays with the first."""
    nest = nest_instance
    victim = _fresh_actor(nest)
    old_id = victim["actor_id_hex"]
    seed_hex = bytes(victim["signing_key"]).hex()
    kit = register_recovery_kit(nest["url"], actor_id_hex=old_id, identity_seed_hex=seed_hex)
    chain = registration_chain(nest["url"], old_id)

    # The second holder signs BEFORE the first lands, over the very chain the
    # first signs over — so nothing about its statement is stale or forged.
    second_seed = secrets.token_bytes(32).hex()
    second_id, second_statement = mint_statement(
        old_actor_id_hex=old_id,
        recovery_secret_hex=kit,
        successor_seed_hex=second_seed,
        registrations=chain,
    )

    first_seed = secrets.token_bytes(32).hex()
    first_id = succeed_identity(
        nest["url"],
        old_actor_id_hex=old_id,
        recovery_secret_hex=kit,
        successor_seed_hex=first_seed,
        old_seed_hex=seed_hex,
    )
    assert first_id != second_id

    with pytest.raises(RpcCallError) as refused:
        submit_statement(nest["url"], second_statement)
    assert refused.value.code == ALREADY_SUCCEEDED, (
        f"a second succession of the same identity must be refused as already "
        f"succeeded, not as a bad statement: {refused.value!r}"
    )

    assert len(succession_statements(nest["url"], old_id)) == 1, (
        "the nest must hold exactly the first succession"
    )
    assert _admin_user_row(nest, first_id) is not None, "the first successor holds the account"
    assert _admin_user_row(nest, second_id) is None, (
        "the later holder's key must not have become an account"
    )
    # The first successor still signs in: the refused attempt took nothing away.
    mint_token_via_handshake(nest["url"], SigningKey(bytes.fromhex(first_seed)))


@pytest.mark.feature("take-your-account-back")
def test_an_admins_suspension_carries_to_the_successor(nest_instance):
    """Suspend the account, then take it back: the successor is suspended too
    and cannot sign in — a new key does not launder a moderation verdict."""
    nest = nest_instance
    member = _fresh_actor(nest)
    old_id = member["actor_id_hex"]
    seed_hex = bytes(member["signing_key"]).hex()
    # The kit is registered while the account may still act (USER-class door).
    kit = register_recovery_kit(nest["url"], actor_id_hex=old_id, identity_seed_hex=seed_hex)

    with admin_client(nest) as admin:
        admin.call(
            "fauna.admin.users.suspend",
            {"actor_id": bytes.fromhex(old_id), "reason": "abuse", "category": "abuse"},
        )
    assert _admin_user_row(nest, old_id)["suspended"], "the precondition: suspended"

    successor_seed = secrets.token_bytes(32).hex()
    successor_id = succeed_identity(
        nest["url"],
        old_actor_id_hex=old_id,
        recovery_secret_hex=kit,
        successor_seed_hex=successor_seed,
    )

    row = _admin_user_row(nest, successor_id)
    assert row is not None, "the succession must have created the successor's account"
    assert row["suspended"], f"the suspension must carry to the successor: {row!r}"
    with pytest.raises(RpcCallError):
        mint_token_via_handshake(nest["url"], SigningKey(bytes.fromhex(successor_seed)))
