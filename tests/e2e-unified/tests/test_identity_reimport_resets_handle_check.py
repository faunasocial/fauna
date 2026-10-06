"""Importing a different key restarts the handle check from scratch — the
cross-app witness.

`docs/features/identity-on-another-device.md` outcome 3;
`docs/goal/behavior/onboarding.md` § 2. Handle entry (the reset is the shared
machine's, so one unmarked test witnesses every column — minted by the
wide-parity disposition pass, which found the outcome
witnessed only by web's richer `test_onboarding_handle_check_reset.py`, a
`local_web`-harness journey no other column can run).

The swap must not carry a concluded verdict across identities: a verdict is a
statement about *this* key on *that* nest, and a user who imports a different
key while an "all good, continue" verdict is showing would otherwise sail into
a nest binding the new key never checked. The web journey proves the live-probe
version (admin A's `AlreadyOnNest` verdict swapped away); this file pins the
same machine reset through every app's own wizard UI, with the concluded
verdict injected (fixture setup — the `test_handle_entry_outcomes.py` seam) so
the case needs no claimed nest and runs identically on all seven columns.

The pre-swap assertion (Continue enabled under the injected verdict) is the
vacuity guard: if the injection ever stops reaching the page, the test fails
there rather than passing an empty reset.
"""

from __future__ import annotations

import time

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot

pytestmark = pytest.mark.tier_2

_SECRET_A = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"  # gitleaks:allow
_SECRET_B = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"  # gitleaks:allow


@pytest.mark.feature("identity-on-another-device")
def test_reimporting_a_different_key_restarts_the_handle_check(app):
    # ── Identity A at handle entry, with a concluded, Continue-enabling
    # verdict on the page. ────────────────────────────────────────────────
    app.onboarding.go_to_handle_entry(secret_hex=_SECRET_A)
    # The setter seeds `current_handle` machine-side itself; typing into the
    # UI here instead would race the injection — an input edit processed after
    # the snapshot lands is a handle CHANGE, and changing the handle is its own
    # reset trigger.
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": {"DomainAvailable": {"buyable_via_provider": True, "price": None}},
        "message": {
            "key": "onboarding.handle_check.outcome.domain_available_unpriced",
            "args": {"domain": "example.com"},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    # Deadline-polled (convention 14): the injection lands in the machine and
    # the page repaints on the app's own tick, so a one-shot read can see the
    # pre-injection frame.
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if app.driver.is_enabled("handle-entry-continue-button"):
            break
        time.sleep(0.3)
    assert app.driver.is_enabled("handle-entry-continue-button"), (
        "vacuity guard: the injected concluded verdict must enable Continue "
        "before the swap, or the reset below would be asserted against nothing: "
        f"{app.driver.diagnose('handle-entry-continue-button')} "
        f"error={app.error_text()!r}"
    )
    verdict_text = (
        app.driver.get_text("handle-message-area")
        if app.driver.is_visible("handle-message-area")
        else ""
    )
    assert verdict_text, (
        "vacuity guard: the injected verdict must render its message before "
        "the swap, so the reset below can assert it is gone"
    )

    # ── Back to the identity stage and import a DIFFERENT key. The wizard's
    # back from handle_entry lands on identity_import or identity_choice
    # depending on the app (both legal — test_handle_first_back_buttons.py);
    # handle both without a driver-type branch. ───────────────────────────
    app.driver.click("handle-entry-back-button")
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if (app.driver.is_visible("paste-secret-field")
                or app.driver.is_visible("import-identity-button")):
            break
        time.sleep(0.3)
    if app.driver.is_visible("paste-secret-field"):
        # clear_and_type, not type_text: the paste field can come back still
        # holding identity A's hex, and appending B's would submit a 128-char
        # non-secret the import rightly refuses.
        app.driver.clear_and_type("paste-secret-field", _SECRET_B)
        app.driver.click("import-submit-button")
    else:
        app.onboarding.import_key(_SECRET_B)
    app.driver.wait_for("handle-input", timeout=15)

    # ── The check restarted from scratch: no stale verdict enables Continue,
    # and no stale verdict text is still showing. Deadline-polled — the reset
    # is state, not timing (convention 14). ───────────────────────────────
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if not app.driver.is_enabled("handle-entry-continue-button"):
            break
        time.sleep(0.3)
    assert not app.driver.is_enabled("handle-entry-continue-button"), (
        "identity A's concluded verdict survived the import of identity B — "
        "Continue is still enabled for a key that never ran a handle check: "
        f"{app.driver.diagnose('handle-entry-continue-button')} "
        f"error={app.error_text()!r}"
    )
    # The reset state legitimately renders the Idle GUIDANCE copy ("Enter your
    # handle, then press Check…" — an empty message there is a copy bug, per
    # the snapshot's own idle() doc), so "no text" is the wrong assertion. What
    # must be gone is the pre-swap VERDICT.
    if app.driver.is_visible("handle-message-area"):
        after = app.driver.get_text("handle-message-area")
        assert after != verdict_text, (
            f"a stale handle-check verdict is still rendered after the "
            f"identity swap: {after!r}"
        )
