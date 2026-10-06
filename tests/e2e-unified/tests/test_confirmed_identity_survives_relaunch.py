"""Confirming a created identity records it durably — the next launch finds it.

`docs/features/create-identity.md` outcome 3;
`docs/goal/behavior/onboarding.md` § Long-term store contract + § App-launch
routing (identity present, no nest binding → the launch routes to
``handle_entry``, never back to ``identity_choice``).

The cross-app witness the wide-parity disposition pass found missing: the outcome was witnessed only by linux's
registry-file readback (`test_onboarding.py::
test_confirm_identity_commits_through_the_shared_registry`, linux-marked
because only that driver exposes the credential store as a readable file).
This file witnesses the same promise the way a user meets it, on every
column: create → confirm → force-quit → relaunch → the wizard resumes past
identity choice, because the confirmed identity was found in the durable
store. A launch that lands back on ``create-identity-button`` is the
apple-2026-08-22 data-loss shape (`onboarding.md` § Implementation status,
smoke K's history): a "confirmed" identity the next boot silently lost.

The identity secret is client-only-resident key material, so this is
CLIENT-side durability — the CR-1 idiom applies: the driver must preserve
the client's long-term store across the relaunch, else the assertion is
vacuous and the test skips (the `test_crash_recovery_journeys.py`
precedent, verbatim).
"""

from __future__ import annotations

import time

import pytest

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("create-identity", "identity-protected-on-this-device")
def test_a_confirmed_identity_is_found_by_the_next_launch(app):
    # Vacuity guard (CR-1 precedent): without the store pin the relaunched
    # process gets a fresh store, and "the identity did not survive" would be
    # indistinguishable from "the client never wrote it".
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a "
            "relaunch, so the confirmed-identity durability assertion would "
            "be vacuous"
        )

    # ── Create and CONFIRM an identity through the wizard UI. The confirm is
    # the durable write (`confirm_generated_identity` — entering the screen
    # never commits; only confirming does). ───────────────────────────────
    app.onboarding.navigate_to_status()
    app.onboarding.generate_identity()
    # Confirm writes the durable record (`confirm_generated_identity`). The
    # wizard's next page is the recovery kit (ui.yaml: identity_created →
    # recovery_kit → handle_entry) — but not every app renders it yet, so race
    # the two landings without a driver-type branch (the
    # `resume_or_import_identity` idiom) and confirm the kit when it shows.
    app.driver.click("identity-continue-button")
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if (app.driver.is_visible("recovery-kit-confirm-button")
                or app.driver.is_visible("handle-input")):
            break
        time.sleep(0.5)
    if app.driver.is_visible("recovery-kit-confirm-button"):
        app.onboarding.confirm_recovery_kit()
    else:
        app.driver.wait_for("handle-input", timeout=15)

    # ── Force-quit + relaunch. In-memory state dies here; only the durable
    # store comes back. `hard_reload` is the canonical cross-driver restart
    # (the draft-persistence files' step 3); with no injected session there is
    # nothing for the native replay to replay, and the preserve pin above
    # keeps the store.
    app.driver.hard_reload()

    # ── The launch found the identity: routing lands on handle entry (the
    # (identity, no nest binding) row of § App-launch routing), never back on
    # the create screen. Deadline-polled — state, not timing. ─────────────
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if (app.driver.is_visible("handle-input")
                or app.driver.is_visible("create-identity-button")):
            break
        time.sleep(0.5)
    assert app.driver.is_absent("create-identity-button"), (
        "the relaunch landed back on identity choice — the confirmed identity "
        "was not found in the durable store (the silent-data-loss shape smoke "
        f"K's history documents): {app.driver.diagnose('create-identity-button')} "
        f"error={app.error_text()!r}"
    )
    assert app.driver.is_visible("handle-input"), (
        "the relaunch found the identity but did not resume the wizard at "
        f"handle entry: {app.driver.diagnose('handle-input')} "
        f"error={app.error_text()!r}"
    )
