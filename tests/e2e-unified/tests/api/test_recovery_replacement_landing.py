"""tier_3 API: a seed-alone RecoveryKey replacement lands once its window runs out.

Goal doc: ``docs/goal/behavior/identity-succession.md`` § The RecoveryKey →
*Replacement* (a seed-alone replacement takes effect only after an uncontested
``RECOVERY_REPLACE_GRACE`` window).

The window is 30 days, so the landing is reached through the ``test-hooks``
door ``POST /api/v1/test/recovery/land_due_replacements``
(``helpers.succession.land_due_replacements``), which runs the production
``land_due_replacements(state, now)`` with a later ``now`` — never a
wall-clock wait (convention 14). Every assertion is on state the nest returns.
"""
from __future__ import annotations

import pytest

from common.auth import create_actor_and_register
from helpers.succession import (
    land_due_replacements,
    register_recovery_kit,
    registration_chain,
    replacement_status,
    request_seed_alone_replacement,
)

pytestmark = pytest.mark.tier_3


def test_an_uncontested_seed_alone_replacement_becomes_the_head_after_its_window(
    nest_instance,
):
    """Park a replacement, run the sweep past the window, see it take over.

    Three properties:
      1. a sweep INSIDE the window lands nothing — the door moves the clock, it
         does not bypass the window;
      2. past the window the parked record is appended as the new chain head,
         leaving every earlier link in place;
      3. nothing pends afterwards, so the standing banner clears.
    """
    user = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    actor_id = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()
    url = nest_instance["url"]

    register_recovery_kit(url, actor_id_hex=actor_id, identity_seed_hex=seed_hex)
    chain_before = registration_chain(url, actor_id)
    request_seed_alone_replacement(url, actor_id_hex=actor_id, identity_seed_hex=seed_hex)

    # The beside-control: a sweep one hour in must leave the request parked.
    land_due_replacements(url, advance_secs=3600)
    assert registration_chain(url, actor_id) == chain_before, (
        "a sweep inside the 30-day window must not land the replacement"
    )
    assert (
        replacement_status(url, actor_id_hex=actor_id, identity_seed_hex=seed_hex)
        is not None
    ), "inside the window the replacement must still pend"

    swept = land_due_replacements(url)
    assert swept["landed"] >= 1, f"the due replacement never landed: {swept}"

    chain_after = registration_chain(url, actor_id)
    assert len(chain_after) == len(chain_before) + 1, (
        "landing appends exactly one link — the parked record — to the chain; "
        f"got {len(chain_before)} → {len(chain_after)}"
    )
    assert chain_after[: len(chain_before)] == chain_before, (
        "landing must append, never rewrite the links already registered"
    )
    assert (
        replacement_status(url, actor_id_hex=actor_id, identity_seed_hex=seed_hex)
        is None
    ), "after the landing nothing pends, so the banner must clear"
