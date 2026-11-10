"""Replacing a recovery key needs the old key; the chain is public — tier_3 API E2E.

Witnesses `docs/features/recovery-kit.md` outcome **17**: *"A recovery key can
be replaced only with the old key's co-signature — the seed alone cannot — and
anyone can read an identity's succession chain without an account."* The
rulings are `docs/goal/behavior/identity-succession.md` § The RecoveryKey →
*Replacement* and the registration-chain bullet of § Implementation status
today: `fauna.recovery.registration.submit` accepts a record only on
`SignedRecoveryKeyRegistration::verify` against the head the nest holds, so a
seed thief can sign a structurally perfect record but cannot produce the prior
key's co-signature; `fauna.recovery.registration.chain` and
`fauna.recovery.succession.lookup` are served pre-identity.

**"The seed alone cannot" means: not immediately.** The goal doc's honest-loss
arm parks a seed-alone record for a contestable 30-day window, after which it
lands (`test_recovery_replacement_landing.py` witnesses that). What this module
pins is the immediate door: a seed-alone record presented there is refused,
moves no head, and parks nothing — the only seed-alone road is the loud,
vetoable window.

The outside-observed twin of the conformance pins in
`bins/fauna-nest/tests/conformance_recovery_replacement.rs`: the records are
genuine (minted by the Rust `recovery_fixture`, which mirrors the kit
ceremony), and the chain reads go over a connection that never signs in
(`clients.ws_rpc_anon_client.WsRpcAnonClient`). Fresh actors only, so the
session nest is safe; latency-independent (convention 14).
"""

from __future__ import annotations

import secrets

import pytest

from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register
from helpers.succession import (
    mint_cosigned_registration,
    mint_seed_alone_registration,
    register_recovery_kit,
    registration_chain,
    replacement_status,
    submit_registration,
    succeed_identity,
    succession_statements,
)

pytestmark = pytest.mark.tier_3


def _fresh_with_kit(nest) -> tuple[str, str, str]:
    """A fresh account with one registered kit: ``(actor_id, seed, kit)``."""
    user = create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )
    actor_id = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()
    kit = register_recovery_kit(nest["url"], actor_id_hex=actor_id, identity_seed_hex=seed_hex)
    return actor_id, seed_hex, kit


@pytest.mark.feature("recovery-kit")
def test_a_replacement_the_old_key_co_signs_becomes_the_head_at_once(nest_instance):
    url = nest_instance["url"]
    actor_id, seed_hex, kit = _fresh_with_kit(nest_instance)
    before = registration_chain(url, actor_id)

    _new_kit, record = mint_cosigned_registration(
        url, actor_id_hex=actor_id, identity_seed_hex=seed_hex, prior_secret_hex=kit
    )
    submit_registration(url, actor_id_hex=actor_id, identity_seed_hex=seed_hex, registration=record)

    after = registration_chain(url, actor_id)
    assert after == before + [record], (
        "a co-signed replacement must land at once as the new head, appended "
        f"verbatim: {len(before)} → {len(after)} links"
    )


@pytest.mark.feature("recovery-kit")
def test_the_seed_alone_cannot_replace_a_registered_key_at_once(nest_instance):
    url = nest_instance["url"]
    actor_id, seed_hex, _kit = _fresh_with_kit(nest_instance)
    before = registration_chain(url, actor_id)

    _thief_kit, record = mint_seed_alone_registration(
        url, actor_id_hex=actor_id, identity_seed_hex=seed_hex
    )
    with pytest.raises(RpcCallError) as refused:
        submit_registration(
            url, actor_id_hex=actor_id, identity_seed_hex=seed_hex, registration=record
        )
    assert "verification failed" in str(refused.value), (
        f"the record lacks the prior key's co-signature, so verification must "
        f"refuse it: {refused.value!r}"
    )
    assert registration_chain(url, actor_id) == before, (
        "a refused seed-alone record must not move the head"
    )
    assert (
        replacement_status(url, actor_id_hex=actor_id, identity_seed_hex=seed_hex) is None
    ), "the immediate door parks nothing — the only seed-alone road is the window"


@pytest.mark.feature("recovery-kit")
def test_anyone_reads_an_identitys_chain_without_an_account(nest_instance):
    """Both halves of the chain — the kit registrations and the successions —
    answer a connection that never signed in; an identity with neither answers
    empty, never an error."""
    url = nest_instance["url"]
    actor_id, seed_hex, kit = _fresh_with_kit(nest_instance)

    assert len(registration_chain(url, actor_id)) == 1, "the registered kit is readable anonymously"
    assert succession_statements(url, actor_id) == [], "never succeeded reads empty"

    succeed_identity(
        url,
        old_actor_id_hex=actor_id,
        recovery_secret_hex=kit,
        successor_seed_hex=secrets.token_bytes(32).hex(),
        old_seed_hex=seed_hex,
    )
    assert len(succession_statements(url, actor_id)) == 1, (
        "the succession is readable anonymously"
    )

    stranger = secrets.token_bytes(32).hex()
    assert registration_chain(url, stranger) == []
    assert succession_statements(url, stranger) == []
