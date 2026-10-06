"""tier_3 API: the recovery remedies an owner reaches with NO SESSION AT ALL.

Goal doc: ``docs/goal/behavior/identity-succession.md`` § Enforcement on the
home nest (submission rides a pre-identity kind, exempt from bearer state and
the seed-signed lockout) and § The RecoveryKey → *Replacement* (the current key
vetoes a seed-alone replacement instantly, pre-identity and challenge-gated).

These are slice 5's two remaining proofs, and the tier is the whole point.

**Why this exists when ``conformance_succession.rs`` and
``conformance_recovery_replacement.rs`` already cover the same ceremonies.**
Those suites call ``(meta.handler)(state, actor, bytes)`` directly — they invent
the caller's actor id and hand it to the handler. That harness *cannot observe*
the property these ceremonies are named for: whether a genuinely anonymous
connection is ALLOWED to reach the kind at all. That verdict is made one layer
up, at ``routes.rs``'s connection gate (``if conn.anonymous &&
!is_pre_identity_kind(&kind)`` → refused), against
``bins/fauna-nest/src/pre_identity_allowlist.rs``. A kind dropped from that
allowlist — or one whose handler is registered but whose entry was never added —
keeps every conformance test green while the remedy is unreachable by the only
caller who needs it: the owner whose sessions the thief revoked.

So the conformance suites pin the *decision*; these pin the *reachability*, over
a real nest binary and a real anonymous WebSocket. Neither replaces the other.

Latency-independent by construction (convention 14): every call is a request
with a reply, and every assertion is on returned state. Nothing sleeps and
nothing polls. The lockout is asserted through its own effect (a refused
handshake for the locked-out key), never through elapsed time.
"""
from __future__ import annotations

import secrets

import pytest

from common.auth import create_actor_and_register
from helpers.succession import (
    emergency_lockout,
    register_recovery_kit,
    registration_chain,
    replacement_status,
    request_seed_alone_replacement,
    succeed_identity,
    succession_statements,
    veto_pending_replacement,
)

pytestmark = pytest.mark.tier_3

_HEX32_LEN = 64


def _fresh_actor(nest):
    """A registered actor nobody else's test shares.

    Dedicated for the same reason ``test_succession_fixture.py`` gives: both
    ceremonies here are account-destructive (a succession re-points the account
    and revokes every session; a lockout blocks auth for an hour), so sharing a
    user would break every later test in the run.
    """
    return create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )


@pytest.mark.feature("take-your-account-back")
def test_the_key_holder_vetoes_a_seed_alone_replacement_over_an_anonymous_connection(
    nest_instance,
):
    """The veto ceremony, end to end, with no bearer anywhere in it.

    The scenario the pre-identity design is FOR: someone holding the identity
    seed parks a replacement of the RecoveryKey, and the real owner — who may
    have no working session at all — cancels it with the phrase alone.

    Three properties, none of which the in-process suite can see:
      1. ``replacement.challenge`` and ``replacement.veto`` are reachable from an
         anonymous connection (the allowlist entry is live, not merely written);
      2. the veto actually cancels — ``status`` goes from pending to empty;
      3. the replacement never became the head, so the owner's original kit is
         still the one the chain names.
    """
    user = _fresh_actor(nest_instance)
    actor_id = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()
    url = nest_instance["url"]

    owner_kit = register_recovery_kit(
        url, actor_id_hex=actor_id, identity_seed_hex=seed_hex
    )
    chain_before = registration_chain(url, actor_id)

    would_be_secret, lands_at = request_seed_alone_replacement(
        url, actor_id_hex=actor_id, identity_seed_hex=seed_hex
    )
    assert would_be_secret != owner_kit, (
        "a replacement request must mint a NEW key — one that registers the key "
        "already registered would make the veto meaningless"
    )
    assert lands_at > 0, "the reply must name when the window closes"

    pending = replacement_status(url, actor_id_hex=actor_id, identity_seed_hex=seed_hex)
    assert pending is not None, (
        "the standing banner read must show the parked replacement for the whole "
        "window — a client with nothing to render cannot offer the veto"
    )
    assert int(pending["lands_at"]) == lands_at, (
        "status must describe the same window the request reply named; got "
        f"{pending['lands_at']} vs {lands_at}"
    )

    cancelled = veto_pending_replacement(
        url, actor_id_hex=actor_id, recovery_secret_hex=owner_kit
    )
    assert cancelled is True, (
        "the current RecoveryKey holder vetoed a genuinely pending replacement, "
        "so the nest must report it cancelled — `false` here would mean the "
        "veto reached the nest but contested nothing"
    )

    assert (
        replacement_status(url, actor_id_hex=actor_id, identity_seed_hex=seed_hex)
        is None
    ), "after a successful veto nothing pends, so the banner must clear"

    assert registration_chain(url, actor_id) == chain_before, (
        "a vetoed replacement must leave the chain untouched — the request parks "
        "a record, it never touches the head"
    )


@pytest.mark.feature("take-your-account-back")
def test_a_succession_lands_through_an_active_lockout_with_no_session(nest_instance):
    """The remedy survives the attack's own best defence.

    A thief holding the seed can invoke the seed-signed emergency lockout, which
    blocks auth for the account outright. If the succession kinds needed a
    bearer, the attack would disable its own remedy — so § Enforcement makes
    submission pre-identity and exempt from bearer state.

    This asserts both halves against the real dispatcher: the lockout is really
    in force (the locked-out key is refused at handshake), and the succession
    still lands over an anonymous connection while it is.
    """
    user = _fresh_actor(nest_instance)
    old_actor_id = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()
    url = nest_instance["url"]

    kit_secret = register_recovery_kit(
        url, actor_id_hex=old_actor_id, identity_seed_hex=seed_hex
    )

    # The thief's move: seed-signed, no bearer, nothing the owner can stop.
    emergency_lockout(url, actor_id_hex=old_actor_id, identity_seed_hex=seed_hex)

    # The beside-control. Without it this test would still pass if the lockout
    # silently did nothing, and the whole exemption would go unproven.
    with pytest.raises(Exception):
        replacement_status(
            url, actor_id_hex=old_actor_id, identity_seed_hex=seed_hex
        )

    successor_id = succeed_identity(
        url,
        old_actor_id_hex=old_actor_id,
        recovery_secret_hex=kit_secret,
        successor_seed_hex=secrets.token_bytes(32).hex(),
        old_seed_hex=seed_hex,
    )
    assert len(successor_id) == _HEX32_LEN and successor_id != old_actor_id, (
        "the succession must land through the lockout and name a genuinely new "
        f"actor id; got {successor_id!r}"
    )

    # The last pre-identity recovery kind, closing the anonymous surface: a peer
    # that last saw the old identity must be able to find out where it went
    # without holding an account here. Asserted on the same fixture chain
    # because a lookup is only meaningful once a succession has really landed.
    statements = succession_statements(url, old_actor_id)
    assert len(statements) == 1, (
        "the anonymous catch-up read must serve exactly the one statement that "
        f"just landed, oldest first; got {len(statements)}"
    )
    assert statements[0], "a served statement must be the verbatim bytes, not empty"
