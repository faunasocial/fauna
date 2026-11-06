"""The store seed that lands a launch on ``launch_retry`` with NO session entered —
the gating case of iOS's launch posture, and an ordinary gate on the other apps.

iOS enters optimistically off a stored session, so a saved identity whose nest
refuses the connection (the seed ``launch_retry`` cases use elsewhere) leaves it
in its cached shell, not on the retry surface. It withholds that entry in exactly
one case: a pending-factory-reset slot (``onboarding.md`` § App-launch routing,
the iOS entry-posture paragraph). So this seed adds one — STALE, naming a nest
that answers ``Claimed`` — beside an identity whose saved nest is dead. The
launch machine's boot reconcile probes the slot's nest, reads a stale slot on a
claimed box, clears it, and takes the ordinary rows; the silent challenge then
fails against the dead saved nest and the verdict is
``Offline { transient: true }`` on a launch that entered nothing
(``libs/fauna-launch-machine/src/machine.rs``, the CR-2 reconcile).

A slot whose nest does not answer would NOT do: the reconcile keeps it and
resumes the pre-filled claim page (the ``Unreachable`` arm), never the retry
surface.
"""

from __future__ import annotations

import json
import time

from common.accounts import actor_id_hex
from drivers.base import PlatformDriver


def stale_reset_slot_fields(secret_hex: str, claimed_nest_url: str) -> dict[str, str]:
    """``raw_fields`` for ``NativeLaunchHarness.launch``: the identity's per-actor
    pending-factory-reset record (the one slot the reconcile reads), minted past
    the machine's grace and pointing at ``claimed_nest_url`` — a live, claimed
    nest, so the reconcile clears it rather than resuming the claim."""
    record = {
        "nest_url": claimed_nest_url,
        "handle": "e2e-user",
        "claim_code": "e2e-stale-reset-code",
        "minted_at_secs": int(time.time()) - PlatformDriver.STALE_FACTORY_RESET_AGE_SECS,
    }
    return {f"fauna/{actor_id_hex(secret_hex)}/pending_factory_reset": json.dumps(record)}
