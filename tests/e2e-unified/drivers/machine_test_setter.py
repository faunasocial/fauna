"""Cross-app wrapper around OnboardingMachine snapshot setters.

Uses the per-driver `call_machine_method` to invoke the setters through
whichever bridge the driver is using (FFI, WASM, FlaUI, etc.).

Both helpers also call `set_step_for_test` first so the page actually
renders — the bare snapshot setters mutate the snapshot but don't
transition the wizard's step. Per the E2E bridge contract (tracked
internally), every app's bridge dispatches `set_step_for_test` /
`set_handle_check_snapshot_for_test` / `set_invite_request_snapshot_for_test`.
"""
import json


def set_handle_check_snapshot(app, snap_dict: dict, *, handle: str = "alice@example.com"):
    """Set the wizard's handle-check snapshot for testing.

    Also seeds a `current_handle` (default `alice@example.com`) so the
    Check button — disabled while the input is empty per the target-state
    doc — is enabled in fixtures that represent a post-probe state. Pass
    `handle=""` to fixture the empty-input state explicitly.
    """
    app.driver.call_machine_method(
        "set_step_for_test",
        json.dumps("HandleEntry"),
    )
    app.driver.call_machine_method(
        "set_current_handle",
        json.dumps(handle),
    )
    app.driver.call_machine_method(
        "set_handle_check_snapshot_for_test",
        json.dumps(snap_dict),
    )


def set_invite_request_snapshot(app, snap_dict: dict):
    """Set the wizard's invite-request snapshot for testing."""
    app.driver.call_machine_method(
        "set_step_for_test",
        json.dumps("InviteRequest"),
    )
    app.driver.call_machine_method(
        "set_invite_request_snapshot_for_test",
        json.dumps(snap_dict),
    )


def set_claim_code_snapshot(app, snap_dict: dict):
    """Set the wizard's claim-code snapshot for testing.

    Lands the wizard at ClaimCode so the page renders, then injects the
    provided ClaimCodeSnapshot (state, message, submit_enabled) — see
    libs/fauna-onboarding-machine/src/snapshots/claim_code.rs.
    """
    app.driver.call_machine_method(
        "set_step_for_test",
        json.dumps("ClaimCode"),
    )
    app.driver.call_machine_method(
        "set_claim_code_snapshot_for_test",
        json.dumps(snap_dict),
    )


def set_nat_mode_snapshot(app, snap_dict: dict):
    """Set the wizard's NAT-mode-choice snapshot for testing.

    Lands the wizard at NatModeChoice so the page renders, then injects the
    provided NatModeSnapshot (state, selected_mode, message, submit_enabled) —
    see libs/fauna-onboarding-machine/src/snapshots/nat_mode.rs.

    Note `selected_mode` is the **lowercase** wire form ("public"/"private"):
    NodeMode is `#[serde(rename_all = "snake_case")]`.
    """
    app.driver.call_machine_method(
        "set_step_for_test",
        json.dumps("NatModeChoice"),
    )
    app.driver.call_machine_method(
        "set_nat_mode_snapshot_for_test",
        json.dumps(snap_dict),
    )


def set_recovery_boxes(app, boxes: list[str]):
    """Land the wizard on NestRecovery and inject the custodied box list.

    The step-4 box-recovery UI (box-recovery.md § Recovery UI (step 4)).
    `boxes` is the list of `nest_actor_id` hex strings the shared pre-login
    resolver (this device's own account store joined with a cold read from the
    entered nest — box-recovery.md § The plane-era recovery floor, (b)) would
    produce in production — here injected directly via the machine bridge so
    the tier_2 render/selection tests need no real nest.

    Seeds the boxes BEFORE flipping the step so the page's first render on
    NestRecovery already has the list. `set_recovery_boxes` takes a single
    `Vec<String>` arg, and the two bridge families disagree on how a top-level
    JSON array is delivered:

    - The **WASM** bridge SPREADS a top-level JSON array as N positional args
      (machine.svelte.ts `__fauna_callMachineMethod`), so the single list arg
      must be double-wrapped (`[boxes]`) — the spread then yields one `boxes`
      arg.
    - The **native** bridges (linux/apple/windows/android) pass `json_arg`
      verbatim to the shared `call_machine_method` dispatcher, which decodes it
      straight as `Vec<String>`, so they take the single-wrapped list (a
      double-wrap would decode as `Vec<Vec<String>>` and be dropped).
    """
    arg = [boxes] if app.driver.is_web() else boxes
    app.driver.call_machine_method(
        "set_recovery_boxes",
        json.dumps(arg),
    )
    app.driver.call_machine_method(
        "set_step_for_test",
        json.dumps("NestRecovery"),
    )


def set_provisioning_snapshot(app, snap_dict: dict):
    """Set the wizard's provisioning snapshot for testing.

    Lands the wizard at NestProvisioning so the page renders, then
    injects the provided ProvisioningSnapshot. Mirrors the four-step
    snapshot shape from `libs/fauna-provisioning/src/progress.rs`:
    `overall`, `steps[]` (Domain/Server/Dns/Online in order), plus
    `started_at_ms`/`finished_at_ms`/`result`/`final_error`.
    """
    app.driver.call_machine_method(
        "set_step_for_test",
        json.dumps("NestProvisioning"),
    )
    app.driver.call_machine_method(
        "set_provisioning_snapshot_for_test",
        json.dumps(snap_dict),
    )
