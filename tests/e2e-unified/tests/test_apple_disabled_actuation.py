"""tier_3 e2e: the apple in-process agent must not drive a control the UI disabled.

`InProcessAutomationServer`'s five ACTUATION routes —
`/element/{click,double_click,type,clear,select}` — used to invoke the registered
closure without ever consulting the entry's `isEnabled` predicate. So the apple
harness could activate a control that is `.disabled(...)` on screen: a
harness-only capability with no user analogue, and a silent divergence from web,
whose Playwright `click()` auto-waits for enabled and fails loudly.

This is testing.md convention 11 one layer down — not a *dropped* command, but an
**illegal one silently honoured**. It is the shape that has repeatedly cost this
project multi-session hunts, because the resulting downstream failure ("the row
never appeared", "the create was lost") is indistinguishable from a genuine
product bug. `actions/events.py::_submit_compose` had to *read the enabled flag
itself, before every click*, purely to tell the two apart.

**Why this file asserts mode-dependently instead of skipping.** ~170 `isEnabled:`
registrations exist across the two apple targets, so the refusal shipped staged
while the blast radius was measured. **That staging ended 2026-08-05** — both
targets swept clean (macOS: 4 violating calls on 3 elements, all fixed; iOS: 1
violating call on 1 element, this file's own probe) — so refusal is now the
DEFAULT and `--permissive-actuation` (→ `FAUNA_E2E_PERMISSIVE_ACTUATION`, read by
`FaunaE2E.strictEnabled`) is the opt-out. Both modes are real contracts, and
*both* must stay pinned:

* strict (default) — the call answers HTTP 409 and the driver raises a named
  `RuntimeError`.
* permissive (`--permissive-actuation`) — the control is still driven, but the
  server NSLogs a `DISABLED-ACTUATION` marker naming route/id/index. That marker
  is the whole measurement mechanism: one permissive sweep enumerates every
  offending call site with no new red, which is why the mode outlived the flip
  rather than being deleted with it. **This test is also the sweep's
  known-positive control** — it drives a disabled control on purpose, so its
  marker MUST appear in any permissive sweep's log. A zero-violation sweep whose
  log lacks `id=contact-actor-id-lookup` means the detector never ran, not that
  nothing violated (that exact artifact once reported a whole iOS sweep clean
  while 567 tests had silently skipped).

A mode-gated *skip* would report `s` in a summary line that reads like success
(convention 7's exact complaint), so this test always runs and asserts whichever
contract its run is under — which is also precisely what makes the two-way diff
run meaningful.

**The subject** is `contact-actor-id-lookup`, the contacts find-submit: shared
predicate `{ !findQuery.trimmed.isEmpty }` on BOTH apple targets
(`Fauna-iOS/Views/Contacts/ContactsView.swift:66`,
`Fauna-macOS/Views/Contacts/MacContactListView.swift:41`), disabled by
construction the moment the page loads with an empty query. No seeding, no
multi-account setup, identical id on both apps — priority #1.

tier_3: real `fauna-nest` binary via `logged_in_app`; apple drivers only.
"""
from __future__ import annotations

import pytest

# ⚠ NO `tui` marker, and NOT a silent omission: this file
# pins a bug class specific to `InProcessAutomationServer`'s apple-only actuation
# gate (its five ACTUATION routes used to skip the `isEnabled` check) and its
# `--permissive-actuation`/`DISABLED-ACTUATION` NSLog sweep mechanism — neither
# exists on tui, whose own automation door is the e2e-agent WS-RPC path
# (convention 11), a different mechanism with its own coverage elsewhere. Genuinely
# apple-only; no tui leg to add.
pytestmark = [pytest.mark.tier_3, pytest.mark.macos, pytest.mark.ios]

# tests/e2e-unified/ui.yaml § contacts.
LOOKUP_BUTTON = "contact-actor-id-lookup"
LOOKUP_FIELD = "contact-actor-id-field"

# The marker `InProcessAutomationServer.actuationGate` NSLogs in permissive mode.
DISABLED_MARKER = "DISABLED-ACTUATION"


def _strict(request) -> bool:
    # Refusal is the default; the flag is the opt-OUT (flipped 2026-08-05).
    return not request.config.getoption("--permissive-actuation")


def test_disabled_control_is_refused_or_flagged(logged_in_app, request):
    """A disabled control is never *silently* driven — it is refused (strict) or
    flagged (permissive). Never honoured with no trace, which is the old bug."""
    app = logged_in_app
    if not (app.driver.is_macos() or app.driver.is_ios()):
        pytest.fail(
            "this suite is apple-only by construction (it drives the apple "
            "in-process automation server); --app should have deselected it"
        )

    app.contacts.navigate()
    app.driver.wait_for(LOOKUP_BUTTON)

    # Precondition, asserted rather than assumed: with an empty query the real UI
    # disables this button. If the app ever stops disabling it, the premise of
    # the whole test is gone and we must fail loudly here rather than "pass" by
    # exercising an enabled control.
    assert app.driver.is_enabled(LOOKUP_BUTTON) is False, (
        f"premise broken: {LOOKUP_BUTTON} should be disabled with an empty "
        f"query (shared `isEnabled: {{ !findQuery.trimmed.isEmpty }}` on both "
        f"apple targets), but reads enabled. "
        f"{app.driver.diagnose(LOOKUP_BUTTON, attrs=('enabled',))}"
    )

    if _strict(request):
        # Loud and NAMED — convention 11 forbids a bare return, a `.debug` log,
        # or a generic failure the reader cannot act on.
        with pytest.raises(RuntimeError) as excinfo:
            app.driver.click(LOOKUP_BUTTON)
        message = str(excinfo.value)
        assert "409" in message, f"the refusal must be an HTTP 409: {message}"
        assert "element is disabled" in message, message
        assert LOOKUP_BUTTON in message, (
            f"the refusal must name the element the test asked for: {message}"
        )
    else:
        # Permissive: still driven (that is the point of staging), but the call
        # must leave a countable trace.
        app.driver.click(LOOKUP_BUTTON)
        _assert_marker_logged(app, LOOKUP_BUTTON, "click")


def test_enabled_controls_are_untouched_by_the_gate(logged_in_app, request):
    """The gate is PRECISE: an enabled control is driven exactly as before.

    This is the regression that would matter most. The gate sits on the hot path
    of every `click`/`type`/`clear`/`select` the apple harness issues, so an
    over-broad predicate (refusing entries that register no `isEnabled`, or
    reading the predicate once at registration instead of live) would not fail
    this one test — it would fail hundreds, in ways that read as product bugs.
    So: type into the enabled field, watch the same button flip to enabled, and
    drive it for real.
    """
    app = logged_in_app
    app.contacts.navigate()
    app.driver.wait_for(LOOKUP_BUTTON)

    # Typing into an enabled field must be unaffected (the `type` route is gated
    # too, and a gate that broke ordinary typing would be worse than the bug).
    app.driver.clear_and_type(LOOKUP_FIELD, "premise-check")
    assert app.driver.get_text(LOOKUP_FIELD) == "premise-check"

    # The SAME control the first test found disabled is now enabled — proving the
    # predicate is re-read live, not captured at registration. A snapshot-reading
    # gate would keep refusing this click forever.
    assert app.driver.is_enabled(LOOKUP_BUTTON) is True, (
        f"a non-empty query must enable {LOOKUP_BUTTON}: "
        f"{app.driver.diagnose(LOOKUP_BUTTON, attrs=('enabled',))}"
    )

    # Honoured in BOTH modes — strict mode refuses disabled controls, never
    # enabled ones. The absence of a raise IS the assertion: under
    # the default this line is the proof that the gate does not over-refuse, and
    # under `--permissive-actuation` it is the proof it does not break the
    # ordinary path.
    #
    # (Deliberately NOT also asserting "no new marker in stderr": the log is
    # append-only across the session-scoped app, so "new" needs a boundary the
    # app has no reason to write, and a substring check against the whole log
    # would fail on the FIRST test's legitimate marker. A check that can only be
    # written unsoundly is better left out than faked.)
    app.driver.click(LOOKUP_BUTTON)


def _assert_marker_logged(app, element_id: str, route: str) -> None:
    """Assert the permissive-mode marker reached the app's captured stderr.

    Only macOS captures the app's stderr (`drivers/macos.py::app_stderr_text`);
    the iOS simulator's app log is not plumbed into the driver. That is a real
    capability difference in the *driver*, not an app-gated skip: the behavioural
    assertion above already ran on both apps, and this is a strictly-stronger
    extra check where the plumbing exists.
    """
    reader = getattr(app.driver, "app_stderr_text", None)
    if reader is None:
        return
    stderr = reader()
    assert DISABLED_MARKER in stderr, (
        f"permissive mode must leave a countable trace: no {DISABLED_MARKER!r} "
        f"marker in the app's stderr after driving a disabled {element_id}. "
        f"Without it, one run cannot enumerate the offenders and the staging "
        f"plan has no measurement. stderr tail: {stderr[-1200:]}"
    )
    assert f"{route} id={element_id}" in stderr, (
        f"the marker must name the route and id so the offender list is "
        f"actionable. stderr tail: {stderr[-1200:]}"
    )
