"""tier_3: a provisioning run that never finished is RESUMABLE from the slot.

`docs/goal/behavior/onboarding.md` § 6 *The pending-provision slot*: the wizard
persists `{nest_url, handle, claim_code, dns_records: []}` **before**
`create_server` and completes `reach_ipv4` the moment it returns, so a user who
quits or crashes mid-run comes back to a box they can still claim — with a code
that exists nowhere else, since the box minted nothing and the run rendered
nothing. `nest/common.md` § Client-state recoverability is the invariant behind
it: no state a client can reach may need an off-box fix, a client crash at any
point included.

The slot's own unit coverage (`pending_provision_slot.rs`) proves the write. What
these two journeys prove is the half no unit test can reach: that a *relaunched
app* finds it and paints something the user can act on. Both assert through the
launch routing, and reaching the surface at all IS the slot assertion — the route
is gated on `identity.is_some() && load_awaiting_dns().is_some()`
(`fauna-launch-machine/src/machine.rs`), so the "Almost ready" page cannot appear
unless the slot survived. The identity comes from the same write: the slot's
writer registers the account (`write_awaiting_dns` → `add_account`; on the
empty registry these journeys seed, the first account is active by rule — the
store path itself never moves the active pointer, which is what keeps an "Add
account" run from hijacking its live session, `test_add_account_provisioning.py`),
which is what makes an interrupted first run resumable at all.

**Two interruptions, one surface, and the difference is the point.** Journey 1
crashes during `Online`, when the box exists and the slot carries its address.
Journey 2 fails `create_server`, when the box never existed at all — the slot
still describes a machine that was ordered and never came, which is the window
the write's placement *before* `create_server` exists for. If custody stopped
preceding dispatch, journey 2 is the one that goes red.

**⚠ What these deliberately do NOT assert: the resumed claim completing.** The
re-armed reach dials `reach_ipv4` on **port 443** — `resolve_override_addr`
hard-codes it, and after a relaunch the armed host is derived from the slot's own
`nest_url`, so the host match is true by construction and the poll is redirected
there every time. Nothing on a dev box serves :443, and no `provider_base_urls`
override escapes it (the override URL's host is the same loopback the guard
matched). That last inch needs a box answering on :443 — the live provision
run — so it stays covered by the unit pins step 5
landed, and a journey here that waited for `nat_mode_choice` would hang on the
poll and read as a product bug.
"""
from __future__ import annotations

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.budgets import APP_RELAUNCH_S, ORCHESTRATION_STEP_S, UI_SETTLE_S
from helpers.provisioning_drive import (
    HANDLE_DOMAIN,
    assert_records_less_almost_ready,
    overall,
    point_providers_at,
    require_durable_client_store,
    seed_standard_path_run,
    step_status,
)
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.crash_recovery]


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_a_crash_during_online_resumes_on_the_records_less_surface(
    killable_app, fake_cloud, provision_target_nest
):
    """SIGKILL during `Online`; the relaunched app resumes on "Almost ready".

    The box is up but never claimed — the run is parked in the Online step's
    retry backoff against a nest that is deliberately *stopped*, which is the
    state that step exists to wait out and a deterministic window to crash in
    (a causal state, not a sleep: the step reports `Running` before the kill).

    By then the slot holds a complete row: written before `create_server` and
    completed with the address the moment it returned. The relaunch must find
    it and paint the surface — no code to type, nothing lost, no off-box fix.
    """
    app = killable_app
    require_durable_client_store(app)

    point_providers_at(
        app, fake_cloud, nest_base_url=provision_target_nest["url"], nest=provision_target_nest
    )
    # The box is not up: the Online step's health poll fails and parks in its
    # retry backoff. Stopping it *after* the override read-back so the run is
    # already pointed at it, and leaving it down for the rest of the journey —
    # this test never wants Online to succeed.
    from common.nest import stop_nest
    stop_nest(provision_target_nest)

    seed_standard_path_run(
        app, handle_domain=HANDLE_DOMAIN,
        claim_code=provision_target_nest["claim_code"],
    )
    app.click("provisioning-start-button")

    wait_until(
        lambda: step_status(app, "Online") == "Running",
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            "the run never reached a Running Online step, so the crash below "
            "would land before the slot was completed and prove nothing about "
            f"resuming one. overall={overall(app)!r}"
        ),
    )

    app.driver.kill_uncleanly()

    # Relaunch via `recover()`, never `hard_reload()`: a real user just reopens
    # the app, and the code under test is the cold launch routing that then runs.
    assert app.driver.recover(), "relaunch after the crash failed"

    assert_records_less_almost_ready(app, after="after a crash during Online,")

    # The surface is actionable, not merely painted: recheck is what will claim
    # the box once it answers, and it must be live at rest.
    #
    # Polled for the same reason the status row is: this button is deliberately
    # disabled while a probe is in flight (single-shot, so a keyboard-mashing
    # user cannot stack probes), and the surface re-probes on its own timer. A
    # single read here would be asking whether the poll happened to be idle at
    # that instant — a wall clock, not a property (convention 14).
    wait_until(
        lambda: app.driver.is_enabled("awaiting-dns-recheck-button"),
        UI_SETTLE_S,
        diagnose=lambda: (
            "the resumed surface never offered a live recheck — it is the "
            "affordance that finishes the interrupted setup: "
            + app.driver.diagnose("awaiting-dns-recheck-button")
        ),
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_create_server_failing_after_the_slot_write_still_resumes(app, fake_cloud):
    """`POST /servers` 500s; the slot is there anyway, and the app resumes on it.

    This is custody-precedes-dispatch stated as a journey. The write happens
    **before** `create_server`, so a provider that fails at exactly that call
    leaves a client holding a claim code for a box that does not exist — and
    that is the correct, recoverable state: the user comes back to the "Almost
    ready" surface rather than to a wizard that has forgotten the whole run.
    Were the write moved to after `create_server` — the shape that reads more
    natural, since only then is there an address to record — this journey is the
    one that catches it.

    No `provision_target_nest` here: the run dies at the Server step and never
    reaches a health poll, so a real box would be a fixture nothing dials.
    """
    require_durable_client_store(app)

    fake_cloud.hetzner_cloud.create_server_always_fails()
    point_providers_at(app, fake_cloud)
    seed_standard_path_run(app)
    app.click("provisioning-start-button")

    wait_until(
        lambda: step_status(app, "Server") == "Failed",
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            "the Server step never failed, so the fake's 500 did not reach the "
            f"orchestrator and this journey proves nothing. overall={overall(app)!r}"
        ),
    )
    assert overall(app) == "Failed", (
        "a run whose Server step exhausted its attempts is a failed run: "
        f"overall={overall(app)!r}"
    )

    # The user gives up on this sitting and comes back later.
    assert app.driver.recover(), "relaunch after the failed run failed"

    assert_records_less_almost_ready(
        app, after="after `create_server` failed with the slot already written,"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_the_exit_retires_the_slot_so_a_relaunch_no_longer_resumes_the_box(
    app, fake_cloud
):
    """"Use a different nest" clears the slot for good: a cold relaunch after it
    lands on `handle_entry`, not back on the surface.

    The box here never existed (`create_server` 500s), which is the case the exit
    was written for — a slot describing a machine that will never come, on a page
    that would otherwise pin every launch for ever with no in-app way out
    (`nest/common.md` § Client-state recoverability, the `CR-2` shape). The first
    half is the sibling journey above: resume on the records-less surface. What
    is new is the second half, which no in-process test can reach — the click
    took the DURABLE slot with it. The launch routing takes the surface only on
    `identity && awaiting-dns slot` (`fauna-launch-machine/src/machine.rs`), so
    "the surface did not come back" IS the slot assertion, and the positive
    landing (`handle-input`) is what proves the relaunch settled somewhere
    rather than the assertion racing a page that has not painted yet
    (convention 14).
    """
    if app_name(app.driver) not in ("tui", "windows"):
        skip_unbuilt(
            app.driver,
            surface="awaiting-dns-fallthrough-button",
            detail="the Almost-ready exit this whole journey drives landed on tui (lead app) 2026-09-25, then windows",
            tracked="onboarding-provisioning.md § \"Almost ready\" surface → Exit",
        )
    require_durable_client_store(app)

    fake_cloud.hetzner_cloud.create_server_always_fails()
    point_providers_at(app, fake_cloud)
    seed_standard_path_run(app)
    app.click("provisioning-start-button")
    wait_until(
        lambda: step_status(app, "Server") == "Failed",
        ORCHESTRATION_STEP_S,
        diagnose=lambda: (
            "the Server step never failed, so there is no slot for the exit to "
            f"retire and this journey proves nothing. overall={overall(app)!r}"
        ),
    )
    assert app.driver.recover(), "relaunch after the failed run failed"
    assert_records_less_almost_ready(
        app, after="before the exit, on the resumed surface,"
    )

    # Polled, not read once: the surface re-probes on its own timer, and the exit
    # is the one control that must stay live through a probe of a box that never
    # answers — but it is off under a claim, so wait for the state that is on.
    wait_until(
        lambda: app.driver.is_enabled("awaiting-dns-fallthrough-button"),
        UI_SETTLE_S,
        diagnose=lambda: (
            "the resumed surface never offered its exit: "
            + app.driver.diagnose("awaiting-dns-fallthrough-button")
        ),
    )
    app.click("awaiting-dns-fallthrough-button")
    app.driver.wait_for("handle-input", timeout=UI_SETTLE_S)

    # The user reopens the app later. The identity is still on the device (the
    # exit changes the nest, not the self); the slot is not.
    assert app.driver.recover(), "relaunch after the exit failed"
    try:
        app.driver.wait_for("handle-input", timeout=APP_RELAUNCH_S)
    except Exception as exc:
        raise AssertionError(
            "the relaunch after the exit did not land on handle_entry — with the "
            "identity kept and the slot cleared the launch routing takes its "
            "`(identity, no nest, no pending)` row. If `awaiting-dns-status` is "
            "what rendered instead, the exit cleared the machine but not the "
            f"durable slot: status visible={app.driver.is_visible('awaiting-dns-status')}"
        ) from exc
    assert app.driver.is_absent("awaiting-dns-status"), (
        "the surface came back after the exit, so the slot survived it"
    )
