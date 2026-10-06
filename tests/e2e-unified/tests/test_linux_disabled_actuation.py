"""tier_3 e2e: the linux in-process agent must not drive a control the UI disabled.

`automation::agent::perform`'s five ACTUATION routes —
`/element/{click,double_click,type,clear,select}` — resolved a GTK widget and
drove it without ever reading `find::is_enabled`, so the linux harness could
activate a `set_sensitive(false)` control: a harness-only capability with no user
analogue, and a silent divergence from web, whose Playwright `click()` auto-waits
for enabled and fails loudly.

This is `e2e-conventions.md` convention 11 **one layer down** — not a *dropped*
command but an **illegal one silently honoured**. The shape has repeatedly cost
this project multi-session hunts, because the downstream failure ("the restore
never happened", "the row never appeared") is indistinguishable from a genuine
product bug. apple ran ungated until 2026-08-02 and its first gated sweep
immediately caught a real one: a feed form whose create button was impossible to
reach through the real UI, while the test driving it directly had passed for
months. **A harness that can do the impossible does not just miss bugs; it
manufactures false evidence that the feature works.**

**linux REFUSES by default since 2026-09-10** (`agent.rs::LINUX_REFUSES_DISABLED_ACTUATION_BY_DEFAULT`).
It staged first, in the order convention 11 prescribes: land the refusal, sweep
the whole suite PERMISSIVELY so one run enumerates every offender with no new
red, triage that list to empty, "and only then make refusal the default" — the
2026-09-10 sweep found the gate's own probe as the only violating call. A strict
sweep would report at most the first offender per test — a failed test stops —
and hide the rest, which is why `--permissive-actuation` survives the flip as
the opt-out: under it the control is still driven and the violation is *marked*.

**This file is the sweep's known-positive control.** It drives a disabled control
on purpose, so its marker MUST appear in any permissive sweep's log. A
zero-violation sweep whose log lacks `id=restore-confirm-button` means the
detector never ran, not that nothing violated — apple's first iOS sweep reported
a whole target clean while 567 of its tests had silently skipped, and the probe's
absent marker is the only thing that caught it.

**The subject** is `restore-confirm-button` (`views/backups/restore.rs:171-179`):
a friction bar that is `set_sensitive(false)` **at build time** and arms only when
the typed text exactly equals the selected snapshot's id. Disabled by
construction the moment the Backups page paints, with no seeding, no snapshot and
no multi-account setup — the cheapest honest disabled control in the app.

tier_3: real `fauna-nest` binary via `logged_in_app`; linux only (it drives the
linux in-process agent's own HTTP surface).
"""
from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

# tests/e2e-unified/ui.yaml § backups.
CONFIRM_BUTTON = "restore-confirm-button"
CONFIRM_INPUT = "restore-confirm-input"

# The marker `fauna_e2e_agent::gate_actuation` logs for every violation. Shared
# verbatim with apple's, so one grep spans a cross-app sweep.
DISABLED_MARKER = "DISABLED-ACTUATION"


def _strict(request) -> bool:
    """Is the app under test refusing, or running permissively?

    Refusal is linux's default, so the run is strict unless
    `--permissive-actuation` opted it out — the same option
    `conftest._apply_actuation_mode_env` turns into
    `FAUNA_E2E_PERMISSIVE_ACTUATION` for the launch, and the same reading apple's
    probe makes.
    """
    return not request.config.getoption("--permissive-actuation")


def test_disabled_control_is_refused_or_flagged(logged_in_app, request):
    """A disabled control is never *silently* driven — it is refused (strict) or
    flagged (permissive). Never honoured with no trace, which is the old bug."""
    app = logged_in_app
    if not app.driver.is_linux():
        pytest.fail(
            "this suite drives the linux in-process agent by construction; "
            "--app should have deselected it"
        )

    app.backups.navigate()
    app.driver.wait_for(CONFIRM_BUTTON)

    # Precondition, asserted rather than assumed: the friction bar builds
    # disabled. If the app ever stops disabling it, the premise of the whole
    # test is gone and we must fail loudly here rather than "pass" by exercising
    # an enabled control.
    assert app.driver.is_enabled(CONFIRM_BUTTON) is False, (
        f"premise broken: {CONFIRM_BUTTON} builds `set_sensitive(false)` and "
        f"arms only when the typed text equals the selected snapshot id "
        f"(views/backups/restore.rs), but reads enabled. "
        f"{app.driver.diagnose(CONFIRM_BUTTON, attrs=('enabled',))}"
    )

    if _strict(request):
        # Loud and NAMED — convention 11 forbids a bare return, a `.debug` log,
        # or a generic failure the reader cannot act on.
        with pytest.raises(RuntimeError) as excinfo:
            app.driver.click(CONFIRM_BUTTON)
        message = str(excinfo.value)
        assert "409" in message, (
            f"the refusal must be a 409: a 404 would send the driver into its "
            f"scroll-retry loop and report 'not rendered yet', the opposite "
            f"diagnosis. Got: {message}"
        )
        assert "element is disabled" in message, message
        assert CONFIRM_BUTTON in message, (
            f"the refusal must name the element the test asked for: {message}"
        )
    else:
        # Permissive: still driven (that is the point of staging), but the call
        # must leave a countable trace.
        app.driver.click(CONFIRM_BUTTON)
        _assert_marker_logged(app, CONFIRM_BUTTON, "click")


def test_enabled_controls_are_untouched_by_the_gate(logged_in_app):
    """The gate is PRECISE: an enabled control is driven exactly as before.

    This is the regression that would matter most. The gate sits on the hot path
    of every click/type/clear/select the linux harness issues, so an over-broad
    predicate — reading the widget's own `sensitive` flag when an ancestor is the
    one greyed out, or reading it once instead of live — would not fail one test,
    it would fail hundreds, in ways that read as product bugs.

    The `type` route is the subject deliberately: gating click alone is not
    compliance (typing into a disabled field is the same illegal act), so `type`
    is gated too, and a gate that broke ordinary typing would be worse than the
    bug it fixes.
    """
    app = logged_in_app
    app.backups.navigate()
    app.driver.wait_for(CONFIRM_INPUT)

    assert app.driver.is_enabled(CONFIRM_INPUT) is True, (
        f"premise: the friction-bar input is always typeable — only the button "
        f"it arms is gated. {app.driver.diagnose(CONFIRM_INPUT, attrs=('enabled',))}"
    )

    # Drives `clear` then `type`, both gated routes, on an enabled control. The
    # read-back is the proof the keystrokes actually landed rather than being
    # swallowed by an over-broad refusal.
    app.backups.type_restore_confirm("premise-check")
    assert app.driver.get_text(CONFIRM_INPUT) == "premise-check"


def _assert_marker_logged(app, element_id: str, route: str) -> None:
    """Assert the permissive-mode marker reached this launch's captured stderr.

    `app.err` is the launch-scoped witness (`drivers/linux.py::app_stderr_text`);
    the run-scoped `--actuation-log` file is the sweep's harvest, and it is fed
    by the same one `gate_actuation` call, so pinning either pins both. stderr is
    the one available without a flag, which is what makes this test meaningful in
    an ordinary `--app linux` run rather than only inside a sweep.
    """
    stderr = app.driver.app_stderr_text()
    assert DISABLED_MARKER in stderr, (
        f"permissive mode must leave a countable trace: no {DISABLED_MARKER!r} "
        f"marker in the app's stderr after driving a disabled {element_id}. "
        f"Without it, one run cannot enumerate the offenders and the staging "
        f"plan has no measurement. stderr tail: {stderr[-1200:]}"
    )
    assert f"{route} id={element_id}" in stderr, (
        f"the marker must name the route and the element, or a sweep's log "
        f"cannot be triaged per element. stderr tail: {stderr[-1200:]}"
    )
