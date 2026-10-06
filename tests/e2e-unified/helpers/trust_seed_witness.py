"""The witness that an app's e2e trust seed reaches a nest: a generation-sealed row there.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The e2e trust seed.
A plaintext e2e nest never graduates the TLS pin an app's escrow trust reads, so
the harness seeds that trust at launch, keyed by nest. An app holding no seed for
the nest it talks to has an empty trust set there, its generation plane is
dormant, and nothing goes red — so a test that means to show the seed arrived
has to look at the plane. This module looks at it; its consumers are
`tests/test_r14_trust_seed_dedicated_nest.py` (a login seam's relaunch, and an
app the test launches itself) and
`tests/test_crash_recovery_journeys.py` (a crash journey's relaunch before the
kill).

**The observable is a GENERATION-SEALED row on the nest's own fleet-only plane
for the signed-in actor** (`fauna.sync.changes.list`, `item_class`
`state-entry`, `scope` `state-fleet`, read through
`common.auth.account_state_changes`). Not just any row there: the generation
machinery itself — the device set, the escrow target, the mint, its wraps and
receipt — is sealed under the account's gen-0 keys (`merge_policy.rs`,
`SealingEpoch::Gen0`), so an account's first pass fills that plane whether or
not a tip ever resolves. (A first draft of the dedicated-nest witness counted
every row and went green on a dormant nest.) A tip-sealed kind
(`SealingEpoch::GenerationTip` — the device-endpoints row every first pass
publishes) seals envelope form v2, which names its generation in the clear, and
the writer door seals one only under an admissible, escrow-acked tip. So a v2
envelope on the nest (`common.auth.is_generation_sealed`) means a tip resolved.
It is read from the nest, so it holds whichever co-located process won the
engine lock — the app or its sync agent (`helpers.waiting.account_pump_role`) —
and whichever launch of the app wrote it.

Convention 14: one named budget, a deadline poll over nest state, and pokes of
the pump only where this app holds the role.
"""

from __future__ import annotations

import time

from common.auth import account_state_changes, is_generation_sealed
from helpers.waiting import (
    account_pump_cycles,
    account_pump_role,
    poke_account_pump,
    wait_until,
)

TRUST_SEED_ENV = "FAUNA_E2E_TRUST_NEST_IDENTITY"

PLANE_BUDGET_S = 240.0
"""For a fresh actor's first generation mint to land a tip-sealed row on its
nest's fleet-only plane: runtime assembly, the pump holder's first pass (it runs
at assembly), the mint's escrow round trip and the put. Far above any
non-pathological run; a green one pays only what the mint costs."""

POKE_SPACING_S = 10.0
"""A full pump pass is expensive: the pokes are spaced, the reads are not."""


def fleet_changes(nest: dict, actor: dict) -> list:
    """``actor``'s own fleet-only account-state feed on ``nest``."""
    reply = account_state_changes(
        nest["port"],
        secret_key=actor["signing_key"].encode().hex(),
        scope="state-fleet",
        base_url=nest["url"],
    )
    return list(reply.get("changes") or [])


def await_a_tip_sealed_row(app, nest: dict, actor: dict, *, where: str) -> None:
    """Wait for a tip-sealed row from ``actor`` on ``nest``'s fleet-only plane.

    ``actor`` is any harness actor dict carrying its ``signing_key`` (a
    `_make_user` user, a nest handle's ``admin``); ``where`` names the nest in
    the failure story."""
    last_poke = [0.0]

    def tip_resolved() -> bool:
        if any(is_generation_sealed(c) for c in fleet_changes(nest, actor)):
            return True
        if account_pump_role(app.driver) == (True, True) and (
            time.monotonic() - last_poke[0] > POKE_SPACING_S
        ):
            poke_account_pump(app.driver)
            last_poke[0] = time.monotonic()
        return False

    wait_until(
        tip_resolved, PLANE_BUDGET_S, diagnose=lambda: _story(app, nest, actor, where)
    )


def _story(app, nest: dict, actor: dict, where: str) -> str:
    """Why no generation tip resolved, from everything that can say (convention 6)."""
    from conftest import _nest_id_hex

    environment = app.driver.relaunch_environment()
    seed = (environment or {}).get(TRUST_SEED_ENV)
    try:
        nest_id = _nest_id_hex(nest["port"])
    except Exception as e:  # noqa: BLE001 — a diagnosis must not mask the assert
        nest_id = f"<unreadable: {e!r}>"
    try:
        changes = fleet_changes(nest, actor)
        rows = f"{len(changes)} fleet-only row(s), {sum(map(is_generation_sealed, changes))} tip-sealed"
    except Exception as e:  # noqa: BLE001
        rows = f"<fleet feed unreadable: {e!r}>"
    try:
        log = app.driver.app_stderr_text() or ""
    except Exception as e:  # noqa: BLE001
        log = f"(app log unreadable: {e!r})"
    wanted = (
        "does not trust",
        "Unmintable",
        "device-endpoints",
        "generation",
        "account runtime",
    )
    lines = [line for line in log.splitlines() if any(w in line for w in wanted)]
    return (
        f"no tip-sealed (form v2) row reached {where}'s fleet-only plane for the "
        f"signed-in actor, so no generation tip resolved there: {rows}. Gen-0 rows "
        "without a tip-sealed one mean the runtime ran and minted nothing it "
        "could trust.\n"
        f"  {nest['url']} serves nest_id {nest_id}\n"
        f"  the trust seed the app's next relaunch reads: {seed!r}\n"
        f"  pump role (runtime, holder): {account_pump_role(app.driver)}; "
        f"cycles: {account_pump_cycles(app.driver)}\n"
        "  an entry naming this nest's url with this nest_id is what makes a "
        "mint verifiable here; 'does not trust' in the log means a wrong key "
        "was seeded, 'Unmintable' that none was.\n"
        "--- app log (plane lines) ---\n" + ("\n".join(lines[-40:]) or "(none)")
    )
