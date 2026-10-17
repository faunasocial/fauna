"""The family link across taking a lost account back — full-stack (tier_3) API E2E.

Witnesses `docs/features/family-safety.md` outcome **26** — *"Taking back a
lost account keeps the family link as it was, whether the child or the guardian
lost theirs"* (`docs/goal/behavior/family-safety.md` § Lifecycle gates).

The two sides are carried by two different mechanisms, which is why both are
driven: the ward's link and policy rows move with the ward's identity as plain
per-actor rows, while the guardian's side is re-pointed by its own rule
(`db/successions.rs::rule_the_guardian_family`). One pair walks both, in turn —
the child loses their account and takes it back, then the guardian does — and
after each ceremony both parties read the link from their own side.

A succession revokes every session of the old key, so this module owns its
nest outright rather than sharing the session nest (the same reason
`test_succession_nest_enforcement.py` does).

Process safety: no ``pkill``/``killall``; the nest is torn down by its fixture.
"""

from __future__ import annotations

import os
import secrets

import pytest
from nacl.signing import SigningKey

from helpers.family_wire import (
    admit_adult_directly,
    admit_ward,
    family_status,
    set_ward_policy,
    ward_row,
)
from helpers.succession import register_recovery_kit, succeed_identity

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="module")
def family_succession_nest(request, nest_mode, tmp_path_factory):
    """A nest this module owns outright: successions revoke sessions."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "family-succession-nest"
    )
    yield nest
    cleanup()


def _take_back(nest, identity: dict) -> dict:
    """Run the recovery ceremony for ``identity`` and return the successor's
    identity in the same shape, keyed by a fresh seed."""
    seed_hex = bytes(identity["signing_key"]).hex()
    kit_secret = register_recovery_kit(
        nest["url"], actor_id_hex=identity["actor_id_hex"], identity_seed_hex=seed_hex
    )
    successor_seed = secrets.token_bytes(32)
    successor_hex = succeed_identity(
        nest["url"],
        old_actor_id_hex=identity["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed.hex(),
        old_seed_hex=seed_hex,
    )
    successor = {"signing_key": SigningKey(successor_seed), "actor_id_hex": successor_hex}
    assert bytes(successor["signing_key"].verify_key).hex() == successor_hex, (
        "the ceremony must hand the account to the successor key it was given"
    )
    return successor


def _guardian_of(nest, ward: dict) -> str | None:
    """The guardian the ward reads on its own family status, as hex."""
    status = family_status(nest, ward)
    supervised_by = status.get("supervised_by") or {}
    actor = supervised_by.get("actor_id")
    return bytes(actor).hex() if actor is not None else None


def _assert_linked(nest, guardian: dict, ward: dict, when: str) -> None:
    row = ward_row(family_status(nest, guardian), ward)
    assert row is not None, (
        f"{when}: the guardian's family status must list the child "
        f"{ward['actor_id_hex']}: {family_status(nest, guardian)!r}"
    )
    assert row["policy"]["contact_approval"] is True, (
        f"{when}: the guardian's settings for the child must come through "
        f"unchanged: {row['policy']!r}"
    )
    assert _guardian_of(nest, ward) == guardian["actor_id_hex"], (
        f"{when}: the child must read the same guardian on their own side: "
        f"{family_status(nest, ward)!r}"
    )


@pytest.mark.feature("family-safety")
def test_taking_back_a_lost_account_keeps_the_family_link_on_either_side(
    family_succession_nest,
):
    """Outcome 26: the child's recovery, then the guardian's, each leave the
    link — and the guardian's settings — exactly as they were.

    The policy is tightened first (`contact_approval` on) so "as it was" has
    something to check beyond the bare link: a recovery that re-created the
    link with default settings would pass a link-only assertion.
    """
    nest = family_succession_nest
    suffix = os.urandom(3).hex()
    guardian = admit_adult_directly(nest, f"famsucc-guardian-{suffix}")
    ward = admit_ward(nest, guardian, f"famsucc-ward-{suffix}")
    set_ward_policy(nest, guardian, ward, contact_approval=True)
    _assert_linked(nest, guardian, ward, "before any recovery")

    ward_successor = _take_back(nest, ward)
    assert ward_row(family_status(nest, guardian), ward) is None, (
        "after the child's recovery the guardian must no longer list the "
        "retired identity — the link moved, it was not copied"
    )
    _assert_linked(nest, guardian, ward_successor, "after the child took their account back")

    guardian_successor = _take_back(nest, guardian)
    _assert_linked(
        nest, guardian_successor, ward_successor, "after the guardian took their account back"
    )
