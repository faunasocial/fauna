"""tier_3 e2e: on web, the nest holding your recovery escrow wears a badge
saying so.

`docs/features/nests-and-trust.md` outcome 20; `docs/goal/ui/nests.md` § What
this page is and § Implementation status today (the escrow-holder badge
bullet); the role: `docs/goal/behavior/participants.md` § The participant
model → Roles.

The six native apps are witnessed inside
``test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored``,
which web cannot run (it lacks the custody mint and accept,
``CustodyActions.require_supported``). The badge does not depend on the
ceremony: it is derived from the account's recorded
``fauna.state.escrow-receipt`` rows (``AccountStoreHandle::escrow_holders``,
web's door ``accountEscrowHolders``), which the account runtime's generation
mint writes when it escrows the generation key at the home nest — the default
e2e trust seed names that nest (``conftest._apply_r14_trust_env``; on web the
origin's ``localStorage``). So this signs a browser in, lets its in-tab
runtime run (poked, never cadence-waited), and reads the home row's badge.
"""

from __future__ import annotations

import contextlib

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from conftest import _merge_trust_seed, _nest_id_hex, _seeded_environment
from drivers import create_driver
from helpers.waiting import (
    account_pump_role,
    await_account_runtime_assembled,
    poke_account_pump,
    wait_until,
)
from i18n.strings import S
from tests.test_account_switcher_web import _await_boot_landed

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

BADGE_BUDGET_S = 180.0


@pytest.mark.feature("nests-and-trust")
def test_web_home_nest_wears_the_escrow_holder_badge(nest_instance, spa_url, request):
    actor = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    seed = build_registry_seed(
        [{"actor_id": actor["actor_id_hex"],
          "secret_hex": bytes(actor["signing_key"]).hex(),
          "nest_url": spa_url, "device_id": "escrow-badge-web", "handle": "escrow"}],
        active=actor["actor_id_hex"],
    )
    # The browser reaches the nest through `spa_url`, so the trust seed names
    # that origin too: the seed is keyed by the nest url the runtime dials.
    environment = _seeded_environment(request, nest_instance)
    key = "FAUNA_E2E_TRUST_NEST_IDENTITY"
    if key in environment:
        environment[key] = _merge_trust_seed(
            environment[key], spa_url, _nest_id_hex(nest_instance["port"])
        )

    driver = create_driver("web")
    driver.launch({
        "url": spa_url + "/app/", "seed_credentials": seed, "environment": environment,
    })
    try:
        _await_boot_landed(driver)
        await_account_runtime_assembled(driver)
        app = ActionLayer(driver)

        def badged() -> bool:
            poke_account_pump(driver)
            app.linked_nests.navigate()
            return app.nest_trust.escrow_badge_text(0) == S.nests.escrow_holder_badge

        wait_until(
            badged, BADGE_BUDGET_S, interval=2.0,
            diagnose=lambda: (
                "the home nest never wore the escrow-holder badge — "
                f"badge text {app.nest_trust.escrow_badge_text(0)!r}, "
                f"pump role {account_pump_role(driver)}, "
                f"error={app.error_text()!r}\n"
                + "\n".join(driver.console_log()[-40:])
            ),
        )
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()
