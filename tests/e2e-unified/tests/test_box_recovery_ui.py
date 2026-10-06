"""E2E coverage for the step-4 box-recovery UI (the client recovery wizard
branch).

Two new onboarding pages (box-recovery.md § Recovery UI (step 4) — approved
shape 2026-07-01):

- ``nest_recovery`` (box-selection hub): a ``recover-box-item`` row per box
  the admin custodies (the account plane's ``fauna.state.deployment-seeds``
  rows), plus the two
  re-provision method buttons (cloud / self-hosted), an empty message, and a
  back button. Reached from the ``recover-lost-box-button`` on identity_choice
  (fresh client: routes through identity_import, then handle_entry to connect
  to a SURVIVING nest the admin owns — Q2-A — landing here once that nest
  resolves as already-owned) or the ``launch-recover-button`` on launch_retry
  (surviving device).
- ``recover_selfhosted_instructions`` (self-hosted seed install): the installer
  command carrying ``FAUNA_DEPLOYMENT_SEED``, a copy button, a continue button,
  and a restore CTA that deep-links to the Backups page.

These tests drive the shared onboarding machine's recovery branch
(``begin_recover_lost_box`` / ``seed_identity_for_recovery`` / box selection /
``recover_via_cloud`` / ``recover_via_selfhosted``) through the cross-app
``call_machine_method`` bridge — no real nest. The box list is injected via
``set_recovery_boxes``; in production it is the shared pre-login resolver's
answer — this device's own account store joined with a cold read from the
entered nest (``box-recovery.md`` § The plane-era recovery floor, (b) The
reads), which the tier_3 ``test_box_recovery_two_nest.py`` drives for real.
Rust machine tests in
``libs/fauna-onboarding-machine/tests/nest_recovery_navigation.rs`` cover the
pure transitions.

Web is the reference client; Task E lifts the recovery UI to the other apps,
at which point each test's guard widens to that client. Web + linux + windows +
cli render the pages today; apple and android are the remaining Task E legs.
"""

from __future__ import annotations

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot, set_recovery_boxes
from helpers.app_surface import skip_unbuilt

pytestmark = pytest.mark.tier_2

# Two syntactically-valid 64-hex nest_actor_id values (the shape a real
# ed25519(seed).public produces). Content is irrelevant to the UI — the row
# just renders the id; the seed itself never crosses into the client.
_BOX_A = "aa" * 32
_BOX_B = "bb" * 32

# The deterministic identity the shared onboarding helpers import.
_IMPORT_KEY = "1" * 64


def _require_recovery_ui(app) -> None:
    """Skip on clients whose Task E recovery UI has not landed yet.

    The recovery UI shipped on web first (the reference); Task E lifts it to the
    other apps, widening this guard per client as each lands. Web, linux,
    windows, cli, android, and apple (macos + ios, 2026-07-31) render the pages
    today."""
    if not (
        app.driver.is_web()
        or app.driver.is_linux()
        or app.driver.is_windows()
        or app.driver.is_tui()
        or app.driver.is_android()
        or app.driver.is_macos()
        or app.driver.is_ios()
    ):
        skip_unbuilt(
            app.driver,
            surface="the box-recovery step-4 recovery UI",
            detail="built on web + linux + windows + tui + android + apple",
            tracked="",
        )


# ---------------------------------------------------------------------------
# Entry CTA + recovery-intent routing (the real flow, no injection)
# ---------------------------------------------------------------------------


@pytest.mark.feature("recover-a-lost-nest")
def test_identity_choice_shows_recover_lost_box_button(app):
    """identity_choice surfaces the fresh-client recovery entry CTA."""
    _require_recovery_ui(app)
    app.onboarding.navigate_to_status()
    assert app.is_visible("recover-lost-box-button"), (
        "identity_choice should show the recover-lost-box CTA: "
        f"{app.driver.diagnose('recover-lost-box-button')}"
    )


def test_recover_lost_box_routes_through_import_to_handle_entry(app):
    """recover-lost-box-button → identity_import (recovery intent) → after the
    identity is loaded, land on handle_entry, NOT nest_recovery directly
    (Q2-A, box-recovery.md § Recovery UI (step 4)): the admin still needs to
    connect to a SURVIVING nest they own before the box list is readable."""
    _require_recovery_ui(app)
    app.onboarding.navigate_to_status()
    app.click("recover-lost-box-button")
    # Recovery intent routes through identity_import first (the pre-login box
    # list needs the identity seed: the cold read opens the escrow wraps with
    # it, and the local read is keyed by its actor).
    app.driver.wait_for("paste-secret-field", timeout=15)
    app.driver.type_text("paste-secret-field", _IMPORT_KEY)
    app.click("import-submit-button")
    # Recovery-intent import now lands on handle_entry (Q2-A), same as a
    # normal import — not directly on nest_recovery.
    app.driver.wait_for("handle-input", timeout=15)
    assert app.is_visible("handle-input"), (
        "recovery-intent import should land on handle_entry (handle-input "
        f"canary): {app.driver.diagnose('handle-input')}"
    )
    assert app.is_absent("recover-back-button"), (
        "recovery-intent import must NOT land directly on nest_recovery: "
        f"{app.driver.diagnose('recover-back-button')}"
    )


def test_recovery_handle_already_on_nest_routes_to_nest_recovery(app):
    """On handle_entry under recovery intent, resolving the typed handle/@domain
    to a nest the admin already owns (AlreadyOnNest) lands on nest_recovery,
    NOT Done/LoggedIn (Q2-A): reachability + owning the identity is all the
    pre-login resolver's cold read needs — it writes nothing, so no
    registration on this nest."""
    _require_recovery_ui(app)
    app.onboarding.navigate_to_status()
    app.click("recover-lost-box-button")
    app.driver.wait_for("paste-secret-field", timeout=15)
    app.driver.type_text("paste-secret-field", _IMPORT_KEY)
    app.click("import-submit-button")
    app.driver.wait_for("handle-input", timeout=15)

    # Inject the AlreadyOnNest outcome (a real handle-check probe is a
    # separate concern — tier_3 `test_handle_entry_outcomes`-style coverage
    # already proves the probe itself; this proves the recovery-intent
    # routing decision once that outcome resolves).
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": {"AlreadyOnNest": {
            "handle_matches": True,
            "current_handle": "alice@example.com",
        }},
        "message": {
            "key": "onboarding.handle_check.outcome.already_on_nest_handle_matches",
            "args": {"handle": "alice@example.com"},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    app.click("handle-entry-continue-button")
    app.driver.wait_for("recover-back-button", timeout=15)
    assert app.is_visible("recover-back-button"), (
        "AlreadyOnNest under recovery intent should land on nest_recovery "
        f"(recover-back-button canary): {app.driver.diagnose('recover-back-button')}"
    )


# ---------------------------------------------------------------------------
# nest_recovery — box list + selection + method choice (injected list)
# ---------------------------------------------------------------------------


@pytest.mark.feature("recover-a-lost-nest")
def test_nest_recovery_renders_box_list(app):
    """nest_recovery renders one recover-box-item row per custodied box, plus
    the method buttons and the back button."""
    _require_recovery_ui(app)
    set_recovery_boxes(app, [_BOX_A, _BOX_B])
    app.driver.wait_for("recover-box-item-0", timeout=15)
    assert app.is_visible("recover-box-item-0"), (
        "nest_recovery should render the first box row: "
        f"{app.driver.diagnose('recover-box-item-0')}"
    )
    assert app.is_visible("recover-box-item-1"), (
        "nest_recovery should render the second box row: "
        f"{app.driver.diagnose('recover-box-item-1')}"
    )
    assert app.is_visible("recover-method-cloud-button"), (
        "nest_recovery should render the cloud re-provision button: "
        f"{app.driver.diagnose('recover-method-cloud-button')}"
    )
    assert app.is_visible("recover-method-selfhosted-button"), (
        "nest_recovery should render the self-hosted button: "
        f"{app.driver.diagnose('recover-method-selfhosted-button')}"
    )
    assert app.is_visible("recover-back-button"), (
        "nest_recovery should render the back button: "
        f"{app.driver.diagnose('recover-back-button')}"
    )


def test_nest_recovery_empty_shows_empty_message(app):
    """No custodied boxes → recover-box-empty-message, no rows."""
    _require_recovery_ui(app)
    set_recovery_boxes(app, [])
    app.driver.wait_for("recover-box-empty-message", timeout=15)
    assert app.is_visible("recover-box-empty-message"), (
        "an empty seed map should surface the empty message: "
        f"{app.driver.diagnose('recover-box-empty-message')}"
    )
    assert app.is_absent("recover-box-item-0"), (
        "no box rows should render when the seed map is empty: "
        f"{app.driver.diagnose('recover-box-item-0')}"
    )


def test_nest_recovery_method_buttons_gated_on_selection(app):
    """The method buttons are disabled until a box is selected (recover_via_*
    guards require_selected_recovery_box); selecting a row enables them."""
    _require_recovery_ui(app)
    set_recovery_boxes(app, [_BOX_A, _BOX_B])
    app.driver.wait_for("recover-box-item-0", timeout=15)
    assert not app.is_enabled("recover-method-cloud-button"), (
        "cloud method should be disabled before a box is selected: "
        f"{app.driver.diagnose('recover-method-cloud-button')}"
    )
    assert not app.is_enabled("recover-method-selfhosted-button"), (
        "self-hosted method should be disabled before a box is selected: "
        f"{app.driver.diagnose('recover-method-selfhosted-button')}"
    )
    app.click("recover-box-item-0")
    assert app.is_enabled("recover-method-cloud-button"), (
        "selecting a box should enable the cloud method: "
        f"{app.driver.diagnose('recover-method-cloud-button')}"
    )
    assert app.is_enabled("recover-method-selfhosted-button"), (
        "selecting a box should enable the self-hosted method: "
        f"{app.driver.diagnose('recover-method-selfhosted-button')}"
    )


@pytest.mark.feature("recover-a-lost-nest")
def test_nest_recovery_cloud_method_advances_to_vps_config(app):
    """Selecting a box then clicking cloud re-provision advances to vps_config
    (recovery mode — reuses the existing provisioning surface)."""
    _require_recovery_ui(app)
    set_recovery_boxes(app, [_BOX_A, _BOX_B])
    app.driver.wait_for("recover-box-item-0", timeout=15)
    app.click("recover-box-item-0")
    app.click("recover-method-cloud-button")
    app.driver.wait_for("vps-config-back-button", timeout=15)
    assert app.is_visible("vps-config-back-button"), (
        "cloud re-provision should advance to vps_config (vps-config-back-button "
        f"canary): {app.driver.diagnose('vps-config-back-button')}"
    )


@pytest.mark.feature("recover-a-lost-nest")
def test_nest_recovery_selfhosted_method_advances_to_instructions(app):
    """Selecting a box then clicking self-hosted advances to
    recover_selfhosted_instructions, which renders the installer command, copy
    button, continue button, and restore CTA."""
    _require_recovery_ui(app)
    set_recovery_boxes(app, [_BOX_A, _BOX_B])
    app.driver.wait_for("recover-box-item-0", timeout=15)
    app.click("recover-box-item-0")
    app.click("recover-method-selfhosted-button")
    app.driver.wait_for("recover-selfhosted-continue-button", timeout=15)
    assert app.is_visible("recover-selfhosted-command"), (
        "self-hosted instructions should render the installer command: "
        f"{app.driver.diagnose('recover-selfhosted-command')}"
    )
    assert app.is_visible("recover-selfhosted-copy-button"), (
        "self-hosted instructions should render the copy button: "
        f"{app.driver.diagnose('recover-selfhosted-copy-button')}"
    )
    assert app.is_visible("recover-selfhosted-continue-button"), (
        "self-hosted instructions should render the continue button: "
        f"{app.driver.diagnose('recover-selfhosted-continue-button')}"
    )
    assert app.is_visible("recover-restore-cta"), (
        "self-hosted instructions should render the restore-data CTA: "
        f"{app.driver.diagnose('recover-restore-cta')}"
    )
