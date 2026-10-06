"""tier_3: a standard-path provisioning run BUILDS and CLAIMS the box.

`docs/goal/behavior/onboarding.md` § 6 *Provisioning = build + claim*: "`Succeeded`
on the standard path means the box is built **and claimed**. The claim is the final
substep of `Online` … run by the machine … the instant `/health` answers … From
`Succeeded`, Continue lands on `NatModeChoice`."

Every other test of that sentence proves it against a fixture or a fake. This one
proves it against a **real `fauna-nest` binary**: the run's claim is a real
`fauna.auth.claim_admin` over real WS-RPC, and the assertion that it worked is the
nest's own `claimed`/`admin_exists` flipping — ground truth on the far side of the
wire, not a snapshot the client wrote for itself. Until this existed, "the standard
path claims the box" was pinned only where the box was imaginary.

**How a local nest can stand in for a provisioned box.** The reach address cannot
reach it — both dial mechanisms pin port 443 (`nest_probe_client_resolving`,
`WsNestApi::resolve_override_addr`) and a harness nest is on a random high port —
so the door is the `nest` provider base URL, which covers both legs of the run at
once: the orchestrator's Online health poll targets `{nest_base_url}/api/v1/health`,
and `WsNestApi::resolve()` prefers the same override for the claim. The VPS and DNS
legs stay on the fake cloud, so the run still walks all four steps.

**What this journey deliberately does NOT cover.** With the `nest` override in
place the first-contact identity root is *inert*: it is keyed by the domain, the
resolved URL's host is loopback, and the host-match guard is `.then_some(…)`, so no
root is found and the graduation falls back to TOFU. The injected-seed trust path
(`security.md` § Transport trust, the *Client-provisioned box* row) is therefore
covered by `pending_provision_slot.rs`'s wiremock family and the live provision run,
never here. Nor does this cover the reach *dial* itself, which needs a box answering
on :443.
"""
import json

import pytest

from helpers.budgets import ORCHESTRATION_STEP_S, UI_SETTLE_S
from helpers.crash_recovery import setup_status
from helpers.provisioning_drive import (
    overall,
    point_providers_at,
    seed_standard_path_run,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_a_standard_path_run_claims_the_real_box_and_continues_to_nat_mode(
    app, fake_cloud, provision_target_nest
):
    drv = app.driver
    nest = provision_target_nest

    assert setup_status(nest["url"]).get("claimed") is False, (
        "precondition: the stand-in box must start unclaimed, or this journey "
        "proves nothing about the run's claim"
    )

    # VPS + DNS at the fake cloud; the nest leg — health poll AND claim — at the
    # real binary. The box already exists and already knows its claim code, so
    # the run presents that one rather than minting its own.
    point_providers_at(app, fake_cloud, nest_base_url=nest["url"], nest=nest)
    seed_standard_path_run(app, claim_code=nest["claim_code"])

    app.click("provisioning-start-button")

    # `Succeeded` is the assertion, not a step along the way: on this path it is
    # defined as built AND claimed, so reaching it is already the claim landing.
    last = {"snap": None}

    def succeeded():
        last["snap"] = drv.call_machine_method("provisioning_snapshot")
        return overall(app) == "Succeeded"

    wait_until(
        succeeded,
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "the run never reached Succeeded. snapshot="
        + json.dumps(last["snap"]),
    )

    # Ground truth on the far side of the wire — the client's own snapshot could
    # say anything.
    st = setup_status(nest["url"])
    assert st.get("claimed") is True and st.get("admin_exists") is True, (
        f"the run reported Succeeded but the box is not claimed: {st!r} — "
        "on the standard path Succeeded means built AND claimed (§ 6)"
    )

    # Continue lands on nat_mode_choice, and only because the claim completed:
    # the button is gated on `claim_completed`, not on Succeeded alone.
    assert app.is_enabled("provisioning-continue-button"), (
        "Continue must enable once the box is claimed: "
        + drv.diagnose("provisioning-continue-button")
    )
    app.click("provisioning-continue-button")
    app.wait_for("nat-mode-confirm-button", timeout=UI_SETTLE_S)
