"""tier_3 e2e (web-only): a fresh claim enters the app WITHOUT a session
teardown, and the post-claim serving enablement runs.

`docs/goal/behavior/onboarding.md` § Long-term store contract: the `LoggedIn`
terminal enters the authenticated app. `docs/goal/architecture/security.md`
§ Post-auth surfacing: the sign-in-refused verdict ends a *signed-in* session
— a nest this app was never signed in to refusing an identity it does not yet
know is no such verdict.

**The defect this pins.** On web an identity is
committed before its account exists: importing (or creating) a key runs
`identity.login`, and the layout's actor-change handlers fire for it. With no
home nest stored yet, the WS-RPC singleton dialed the box about to be claimed,
the box answered `fauna.auth.not_registered`, and the client's reconnect loop
stopped for good. The singleton is keyed on (actor, nest URL), neither of which
the claim changes, so the `LoggedIn` terminal's hand-offs ran over that SAME
stopped client: serving enablement failed with `SignInRefusedError`, the
escalation fired for real, and the tab reloaded through the launch route before
it landed on the feed. Nothing retries serving enablement after that reload.

**Observables (convention 14, no settle windows).** The session generation
(`helpers.waiting.session_generation`) is web's teardown counter — persisted in
`sessionStorage`, so it survives the reload it counts and only ever grows. The
serving-enablement completion anchor (`await_serving_enablement_for`) is the
hand-off's own end. The test waits for EITHER the anchor for this actor OR a
teardown, then grades both: a green run has the anchor and an unmoved counter.

The pre-claim dial needs the SPA's dial target to BE the box being claimed —
the self-hosted default, where the nest serves its own SPA. Here that is the
`nest` provider override (`set_provider_base_urls`), which carries both the
wizard's leg and the session's socket, exactly as `test_box_recovery_two_nest.py`'s
web seat sets it up.
"""

from __future__ import annotations

import json

import pytest

from actions import ActionLayer
from conftest import get_available_apps
from drivers import create_driver
from helpers import budgets
from helpers.waiting import serving_enablement, session_generation, wait_until

if "web" not in get_available_apps():
    pytest.skip("web client not selected", allow_module_level=True)

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

#: Only the local part is registered; the `@domain` suffix becomes the claim's
#: `mail_domain` (`wizard_submit_claim_code`).
HANDLE = "admin@nest.test"


@pytest.fixture
def web_driver(spa_url):
    driver = create_driver("web")
    driver.launch({"url": f"{spa_url}/app/"})
    yield driver
    driver.teardown()


def _completed_run_for(driver, actor_id_hex: str):
    """This actor's completed serving-enablement run, or None — tolerating a
    read that lands mid-reload (the very case a red run produces)."""
    try:
        value = serving_enablement(driver)
    except Exception:  # noqa: BLE001 — a destroyed execution context mid-reload.
        return None
    if not value:
        return None
    for run in value["runs"]:
        if str(run.get("actor_id", "")).lower() == actor_id_hex and run.get("completed"):
            return run
    return None


def _generation(driver):
    try:
        return session_generation(driver)
    except Exception:  # noqa: BLE001 — mid-reload.
        return None


def _console_tail(driver) -> str:
    return "\n".join(driver.console_log()[-60:])


@pytest.mark.feature("claim-a-fresh-nest")
def test_an_imported_identity_claims_a_fresh_box_and_enters_without_a_teardown(
    web_driver, provision_target_nest
):
    from nacl.signing import SigningKey

    nest = provision_target_nest
    app = ActionLayer(web_driver)
    nest_url = nest["url"]
    # Re-mounts the page — before any identity exists.
    web_driver.set_provider_base_urls({"nest": nest_url})

    key = SigningKey.generate()
    secret_hex = bytes(key).hex()
    actor_id_hex = bytes(key.verify_key).hex()

    # The identity is committed here, before its account exists: the layout's
    # actor-change handlers fire for it now.
    app.onboarding.navigate_to_status()
    app.onboarding.import_key(secret_hex)
    web_driver.wait_for("handle-input", timeout=15)
    web_driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest", json.dumps([nest_url, HANDLE])
    )
    web_driver.wait_for("claim-code-input", timeout=15)

    before = _generation(web_driver)
    assert before is not None, (
        "web must publish session_generation (convention 14) — without it no "
        "teardown can be ruled out"
    )

    web_driver.clear_and_type("claim-code-input", nest["claim_code"])
    web_driver.click("claim-code-submit-button")
    web_driver.wait_for("nat-mode-confirm-button", timeout=30)
    # Exits the wizard to `LoggedIn`: the terminal's hand-offs fire, then the
    # app is entered.
    app.onboarding.finish_nat_mode()

    def settled():
        generation = _generation(web_driver)
        if generation is not None and generation > before:
            return "teardown"
        if _completed_run_for(web_driver, actor_id_hex):
            return "enabled"
        return None

    outcome = wait_until(
        settled, budgets.SERVING_ENABLEMENT_S,
        diagnose=lambda: f"console tail:\n{_console_tail(web_driver)}",
    )
    after = _generation(web_driver)
    assert outcome == "enabled" and after == before, (
        "a fresh claim must enter the app through the LoggedIn terminal alone: "
        f"session generation {before} -> {after} (a rise is a session teardown "
        "and reload), and the serving-enablement run for this actor "
        f"{'completed' if outcome == 'enabled' else 'never completed'}.\n"
        f"console tail:\n{_console_tail(web_driver)}"
    )
    web_driver.wait_for("feed-view", timeout=budgets.UI_SETTLE_S)
    # Past the anchor: anything the terminal's first signed-in calls could
    # still have escalated has run. Read once more; the counter only grows.
    web_driver.barrier()
    assert _generation(web_driver) == before, (
        "the session tore down after the serving enablement completed.\n"
        f"console tail:\n{_console_tail(web_driver)}"
    )
