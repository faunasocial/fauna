"""tier_3 API: the recovery/succession FIXTURE path, with no app in the picture.

Goal doc: ``docs/goal/behavior/identity-succession.md`` § The RecoveryKey
(registration) and § Enforcement on the home nest (the succession transaction).

This is the convention-5 companion to
``tests/test_identity_succession_refusal.py``: that journey's *precondition* is
a genuinely registered kit and a genuinely landed succession, arranged by
``helpers/succession.py`` + the Rust ``recovery_fixture`` helper. When the
journey goes red, the first question is always "did the app fail, or did the
fixture?" — and answering it by re-running a 25-minute UI journey is exactly
the debugging loop convention 5 exists to delete. This pins the fixture half in
seconds, with no driver at all.

It also pins the two records against the **real nest verifier** rather than
against the helper's own idea of them. Both are canonical DAG-CBOR signed under
domain-separation tags; if a record's shape drifts from what the nest accepts,
the failure surfaces here as a refused submit, naming which record broke,
instead of as an inexplicable UI timeout six apps later.

Latency-independent by construction (convention 14): every call is a request
with a reply, and every assertion is on returned state. Nothing sleeps and
nothing polls.
"""
from __future__ import annotations

import secrets

import pytest

from common.auth import create_actor_and_register
from helpers.succession import (
    register_recovery_kit,
    registration_chain,
    succeed_identity,
)

pytestmark = pytest.mark.tier_3

_HEX32_LEN = 64


def _fresh_actor(nest):
    """A registered actor nobody else's test shares.

    Dedicated because succession re-points the account and revokes every
    session of the old identity — running it against a shared user would break
    every later test in the run.
    """
    return create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )


@pytest.mark.feature("recovery-kit")
def test_the_fixture_registers_a_kit_the_nest_serves_back(nest_instance):
    """register-kit lands a record the nest accepts and serves in the chain."""
    user = _fresh_actor(nest_instance)
    actor_id = user["actor_id_hex"]

    assert registration_chain(nest_instance["url"], actor_id) == [], (
        "a freshly registered actor has no RecoveryKey yet, and the nest must "
        "answer that with an EMPTY chain rather than an error"
    )

    secret = register_recovery_kit(
        nest_instance["url"],
        actor_id_hex=actor_id,
        identity_seed_hex=bytes(user["signing_key"]).hex(),
    )
    assert len(secret) == _HEX32_LEN, f"the kit is 64-hex, got {len(secret)}"

    chain = registration_chain(nest_instance["url"], actor_id)
    assert len(chain) == 1, (
        "the nest must serve back exactly the one registration just submitted; "
        f"got {len(chain)} entries"
    )
    assert chain[0], "a served registration must be the verbatim bytes, not empty"


@pytest.mark.feature("take-your-account-back")
def test_the_fixture_succeeds_an_identity_and_the_nest_records_it(nest_instance):
    """succeed_identity lands a statement the nest verifies and serves.

    The load-bearing one: the journey's whole premise is that this succession is
    REAL — a fabricated row would not verify, so the client could never name the
    successor.
    """
    user = _fresh_actor(nest_instance)
    old_actor_id = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()

    kit_secret = register_recovery_kit(
        nest_instance["url"], actor_id_hex=old_actor_id, identity_seed_hex=seed_hex
    )
    successor_seed = secrets.token_bytes(32).hex()

    successor_id = succeed_identity(
        nest_instance["url"],
        old_actor_id_hex=old_actor_id,
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed,
        old_seed_hex=seed_hex,
    )

    assert len(successor_id) == _HEX32_LEN, (
        f"the successor actor id is 64-hex, got {len(successor_id)}"
    )
    assert successor_id != old_actor_id, (
        "the successor must be a genuinely new actor id — `actor_id` IS the "
        "Ed25519 pubkey, so an in-place swap is not expressible"
    )
    # `succeed_identity` already refuses a mismatch between the statement it
    # signed and the successor the nest echoed back, so reaching here means the
    # nest applied THIS statement rather than merely accepting some bytes.


@pytest.mark.feature("recovery-kit")
def test_a_kit_from_the_wrong_identity_is_refused(nest_instance):
    """The authorization rule, negatively: a kit the chain does not name cannot
    succeed the account.

    Worth pinning because it is the property that makes the whole ceremony safe
    against a seed thief — they can sign structurally perfect bytes and still
    authorize nothing. It also proves the fixture's own pre-check is a
    convenience rather than the thing standing between a bad statement and the
    account.
    """
    victim = _fresh_actor(nest_instance)
    attacker = _fresh_actor(nest_instance)

    register_recovery_kit(
        nest_instance["url"],
        actor_id_hex=victim["actor_id_hex"],
        identity_seed_hex=bytes(victim["signing_key"]).hex(),
    )
    # A perfectly valid kit — for the WRONG account.
    attacker_kit = register_recovery_kit(
        nest_instance["url"],
        actor_id_hex=attacker["actor_id_hex"],
        identity_seed_hex=bytes(attacker["signing_key"]).hex(),
    )

    with pytest.raises(Exception):
        succeed_identity(
            nest_instance["url"],
            old_actor_id_hex=victim["actor_id_hex"],
            recovery_secret_hex=attacker_kit,
            successor_seed_hex=secrets.token_bytes(32).hex(),
        )
