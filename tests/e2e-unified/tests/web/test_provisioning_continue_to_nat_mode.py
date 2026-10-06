"""Web coverage for provisioning-continue -> nat_mode_choice.

`docs/goal/behavior/onboarding.md` § 6 (ratified 2026-08-29): on the standard
path `Succeeded` means the box is built **and claimed**, so Continue lands on
`nat_mode_choice` exactly as a claim-code submit does — this page never exits
straight to `Done`/`LoggedIn`.

**This file used to assert the opposite**, and its own docstring explained why
that was safe: `continue_from_provisioning` was "pure routing … no WS-RPC, no
claim, no auth", so Continue emitted `LoggedIn` and the glue navigated to
`/app/feed`. That is exactly the defect the ratification closes — every real
user of "Set up a nest from the app" was signed in to a box **nobody had
claimed** and then bounced to a claim page asking for a code they never saw. The
old assertion is retired with the premise; the `LoggedIn` glue it exercised is
covered where that exit now lives (`test_onboarding_logged_in_terminal_web.py`,
and the NAT-mode page in `test_nat_mode_choice.py`).

**This file used to drive a real fake-cloud run to `Succeeded` and then stand
in for the claim.** That raced itself: since the machine reopens `Online` for
its own claiming substep after the orchestrator publishes `Succeeded`
(`libs/fauna-onboarding-machine`'s `run_provisioning_claim`), and this
fixture's fake cloud serves `/api/v1/health` over HTTP and nothing over
WS-RPC, `claim_provisioned_box`'s probe cannot succeed — the claim substep
fails, `overall` lands back on `Failed`, and Continue enables only in the
sub-millisecond window between `set_run_succeeded` and `set_claiming`. The fix
is the cross-app twin's shape: reach `Succeeded` by fixture
(`set_provisioning_snapshot`) rather than by orchestrating a run, so there is
no claiming substep to race.

Still a ROUTING-only assertion, and still web-glue-shaped: the claim's *logic*
is Rust-unit-tested (`libs/fauna-onboarding-machine/tests/`
`continue_from_provisioning.rs` + `awaiting_manual_dns.rs`), and the end-to-end
claim against a real box is the tier_3 journey
(`test_provisioning_claims_a_real_nest.py`). What only a web run can show is
that `+page.svelte` renders the next page rather than dropping the
transition — the cross-app twin,
`test_provisioning_progress.py::test_continue_on_a_claimed_succeeded_run_lands_on_nat_mode_choice`,
cannot carry that assertion (no `window.location` on non-web apps), which is
why this file is kept rather than deleted in its favor.

Web-specific (`window.location` reads are a web-only escape hatch via `eval_js`)
— lives in `tests/web/` alongside the other web-specific regression files.
"""
import json

import pytest

from drivers.machine_test_setter import set_provisioning_snapshot

pytestmark = [pytest.mark.web, pytest.mark.tier_2]


def _succeeded_snapshot():
    steps = [
        {
            'kind': kind, 'status': 'Succeeded', 'substep': None,
            'attempt': 0, 'max_attempts': 3, 'last_error': None,
            'skip_reason': None, 'started_at_ms': None, 'finished_at_ms': None,
        }
        for kind in ('Domain', 'Server', 'Dns', 'Online')
    ]
    return {
        'overall': 'Succeeded',
        'steps': steps,
        'started_at_ms': 50_000,
        'finished_at_ms': 80_000,
        'result': {
            'server_id': 'srv-1',
            'ipv4': '203.0.113.10',
            'domain': 'example.test',
            'claim_code': 'claim-xyz',
        },
        'final_error': None,
    }


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_provisioning_continue_lands_on_nat_mode_choice(app):
    """Reach `Succeeded` by fixture (no real orchestrator run), stand in
    for the claim, click Continue, and assert the app renders the NAT-mode
    page — not the feed, and not a dropped transition."""
    drv = app.driver

    drv.call_machine_method("set_current_handle", json.dumps("alice@example.test"))
    set_provisioning_snapshot(app, _succeeded_snapshot())

    # The box a real run would have claimed by now. Continue REFUSES an
    # unclaimed box on this path (that refusal is the defect fix), and this
    # fixture is snapshot-driven with no box here to claim.
    drv.call_machine_method("set_claim_completed_for_test", json.dumps(True))

    app.onboarding.continue_from_provisioning()

    # The wizard stays in the wizard: § 3b-bis owns the exit from here, so the
    # NAT-mode page renders and the app has NOT navigated away.
    pathname_script = "window.location.pathname"
    assert drv.eval_js(pathname_script) != "/app/feed", (
        "Continue must not exit to the feed on the standard path — the box is "
        "claimed here, and the NAT-mode confirm is the terminal step "
        "(onboarding.md § 6). "
        f"got={drv.eval_js(pathname_script)!r} error={app.error_text()!r}"
    )
