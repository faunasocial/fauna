"""E2E coverage for the nat_mode_choice onboarding page (the NAT axis).

The single, terminal admin-path setup step: reached once the admin claim
completes, it resolves the nest's NAT mode (public / private — the
network-reachability axis). `selected_mode` pre-selects the nest's seeded
`node_mode`, refined private-ward when the wizard's target is a private-network
address, so the common case is confirm-only. "Decide later" is visible on every
state and keeps the seed — already a working default, so unlike the retired
storage-mode step there is no unresolved state and no deferral resume loop.

These tests use the test-helpers bridge (set_step_for_test +
set_nat_mode_snapshot) — no real nest, no real fauna.setup.nat_mode commit.
The commit flow itself is covered by the Rust suites:
libs/fauna-onboarding-machine/tests/nat_mode_lifecycle.rs (machine) and
bins/fauna-nest/tests/nat_mode_api.rs (nest).

Per docs/goal/behavior/onboarding.md § 3b-bis.
"""

from drivers.machine_test_setter import set_nat_mode_snapshot

import pytest

pytestmark = pytest.mark.tier_2


def _msg(key, **args):
    return {"key": key, "args": args}


def _snap(state, selected_mode, message_key, submit_enabled, **args):
    return {
        "state": state,
        "selected_mode": selected_mode,
        "message": _msg(message_key, **args),
        "submit_enabled": submit_enabled,
    }


@pytest.mark.feature("claim-a-fresh-nest")
def test_nat_mode_choosing_public_seed(app):
    """Choosing with the common `public` seed: both radios, the status line,
    and both buttons render; confirm is enabled."""
    set_nat_mode_snapshot(app, _snap(
        "Choosing", "public", "onboarding.nat_mode.choosing", True,
    ))
    for element in (
        "public-nat-mode-radio",
        "private-nat-mode-radio",
        "nat-mode-status",
        "nat-mode-confirm-button",
        "nat-mode-defer-button",
    ):
        assert app.is_visible(element), (
            f"Choosing state should show {element}: "
            f"{app.driver.diagnose(element)}"
        )
    assert app.is_enabled("nat-mode-confirm-button"), (
        "confirm should be enabled in Choosing state: "
        f"{app.driver.diagnose('nat-mode-confirm-button')}"
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_nat_mode_private_ward_preselection(app):
    """The private-ward refinement: a private-network target pre-selects
    Private and says why in nat-mode-status. The page must render the
    pre-selection without a click — that's the whole point of the seed."""
    set_nat_mode_snapshot(app, _snap(
        "Choosing", "private", "onboarding.nat_mode.private_hint", True,
    ))
    assert app.is_visible("private-nat-mode-radio"), (
        "the private radio should render: "
        f"{app.driver.diagnose('private-nat-mode-radio')}"
    )
    status = app.get_text("nat-mode-status")
    assert status.strip(), (
        "the private-ward hint should render into nat-mode-status, explaining "
        f"why Private was pre-selected; got {status!r}"
    )
    assert app.is_enabled("nat-mode-confirm-button"), (
        "confirm stays enabled on the pre-selected private seed: "
        f"{app.driver.diagnose('nat-mode-confirm-button')}"
    )


def test_nat_mode_submitting_disables_confirm(app):
    """While the commit is in flight, confirm is disabled (submit_enabled is
    False) — but "Decide later" stays visible, per § 3b-bis."""
    set_nat_mode_snapshot(app, _snap(
        "Submitting", "public", "onboarding.nat_mode.submitting", False,
    ))
    assert app.driver.is_disabled("nat-mode-confirm-button"), (
        "confirm must be disabled while Submitting: "
        f"{app.driver.diagnose('nat-mode-confirm-button')}"
    )
    assert app.is_visible("nat-mode-defer-button"), (
        "the defer button is visible on every state: "
        f"{app.driver.diagnose('nat-mode-defer-button')}"
    )


def test_nat_mode_error_keeps_confirm_enabled(app):
    """A failed commit leaves confirm ENABLED — the fauna.setup.nat_mode set is
    mutable, so resubmit is always allowed (unlike the write-once storage-mode
    commit it mirrors). The cause renders into nat-mode-status."""
    set_nat_mode_snapshot(app, {
        "state": {"Error": {"transient": True, "cause": "connection reset"}},
        "selected_mode": "public",
        "message": _msg("onboarding.nat_mode.error.transient", cause="connection reset"),
        "submit_enabled": True,
    })
    assert app.is_enabled("nat-mode-confirm-button"), (
        "confirm must stay enabled after an error — the set is mutable, so a "
        "resubmit is always allowed: "
        f"{app.driver.diagnose('nat-mode-confirm-button')}"
    )
    status = app.get_text("nat-mode-status")
    assert "connection reset" in status, (
        f"the error cause should surface in nat-mode-status; got {status!r}"
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_nat_mode_selecting_private_updates_selection(app):
    """Clicking the private radio drives select_nat_mode(Private) through the
    shared machine, and the page re-renders off the new snapshot."""
    set_nat_mode_snapshot(app, _snap(
        "Choosing", "public", "onboarding.nat_mode.choosing", True,
    ))
    app.click("private-nat-mode-radio")
    assert app.driver.is_enabled("nat-mode-confirm-button"), (
        "confirm stays enabled after switching to Private: "
        f"{app.driver.diagnose('nat-mode-confirm-button')}"
    )
    assert app.is_visible("nat-mode-confirm-button"), (
        "there is ONE confirm button on this page regardless of the selected "
        "mode (unlike encryption_mode_choice's two): "
        f"{app.driver.diagnose('nat-mode-confirm-button')}"
    )
