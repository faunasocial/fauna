"""tier_3 e2e: the windows FlaUI bridge must not drive a control the UI disabled.

`Actions.cs`'s five ACTUATION routes — `/element/{click,double_click,type,clear,
select}` — resolved a UIA element and drove it without ever reading its
`IsEnabled`, so the windows harness could activate an `IsEnabled="False"` control.
That is `e2e-conventions.md` convention 11 **one layer down** — not a *dropped*
command but an **illegal one silently honoured** — and it presents identically to
a product bug: the call "succeeds", the app does nothing, and the test fails on a
downstream read.

**windows' hole was the PHYSICAL paths, and that is why it survived.** UIA's own
patterns refuse a disabled element (`Invoke`, `SetValue` throw
`ElementNotEnabledException`), so a disabled *button* mostly failed loudly by
accident — as a generic 500 naming no element and no rule, and only after
`Click`'s 750 ms disabled-retry window. But every fallback that reaches
`SendInput` — the physical click, `Type`'s keyboard path, `DoubleClick` outright —
strikes the pixel, runs no handler, and acks 200. A harness that can do the
impossible does not just miss bugs; it **manufactures false evidence that the
feature works** (apple's first gated sweep caught a feed form no user could reach,
whose test had passed for months; linux's caught a real product deadlock).

**windows REFUSES by default since 2026-09-14** (`flaui-bridge/ActuationGate.cs::
WindowsRefusesDisabledActuationByDefault`). It staged first, in the order
convention 11 prescribes: land the refusal, sweep the whole suite PERMISSIVELY so
one run enumerates every offender with no new red, triage that list to empty, and
only then flip. The 2026-09-11 chunked sweep marked three calls: two were this
file's own, and the third was `test_conversation_room_roles.py` asserting, on
purpose, that a plain member's Remove is refused. `--permissive-actuation`
survives the flip as the opt-out: under it the control is still driven and the
violation is *marked*.

**This file is the sweep's known-positive control.** It drives disabled controls
on purpose, so its markers MUST appear in any permissive `--app windows` sweep's
log. A zero-violation sweep whose log lacks `id=restore-confirm-button` means the
detector never ran, not that nothing violated — apple's first iOS sweep reported a
whole target clean while 567 of its tests had silently skipped, and linux's first
TWO sweeps died at this very probe and enumerated nothing. Grade signal 2 before
reading anything else.

**The subjects** all paint on the Backups page of a fresh account, with no
seeding, no snapshot and no destination (`Views/BackupsPage.xaml:244-265`) — the
same three tui's probe uses, which makes the files true twins:

* `restore-source-select` — `IsEnabled="False"`, armed only by a configured
  destination (cross-location pull). A disabled **select**.
* `restore-confirm-button` — `IsEnabled="False"` at build time, armed only when
  the typed text equals the selected snapshot's id, and a fresh account has no
  snapshots. The friction bar linux's probe uses too.
* `restore-confirm-input` — a plain `TextBox` beside them, always enabled: the
  precision control proving the gate did not over-refuse.

**The marker witness is the BRIDGE's stderr, not the app's** — windows' automation
server is an out-of-process bridge, so nothing it decides ever reaches the app's
own log. `drivers/windows.py::bridge_stderr_text` is the analogue of the
linux/tui drivers' `app_stderr_text`.

tier_3: real `fauna-nest` binary via `logged_in_app`; windows only (it drives the
FlaUI bridge's own HTTP surface).
"""

from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# tests/e2e-unified/ui.yaml § backups.
SOURCE_SELECT = "restore-source-select"
CONFIRM_BUTTON = "restore-confirm-button"
CONFIRM_INPUT = "restore-confirm-input"

# The marker `ActuationGate.MarkerLine` writes. Shared verbatim with the Rust
# hosts' and apple's, so one grep spans a cross-app sweep.
DISABLED_MARKER = "DISABLED-ACTUATION"


def _strict(pytestconfig) -> bool:
    """Is refusal switched ON for this run?

    Refusal is windows' default, so the run is strict unless
    `--permissive-actuation` opted it out — the same option
    `conftest._apply_actuation_mode_env` turns into
    `FAUNA_E2E_PERMISSIVE_ACTUATION` for the launch, and the same reading apple's
    and linux's probes make.

    **Reads the pytest option, never `os.environ`, and that is load-bearing**
    (tui paid three attempts for the lesson): the helper writes the flag into the
    per-launch environment dict the bridge receives, never into the pytest
    process's own environment, so an `os.environ` read would disagree with the
    bridge about the mode.
    """
    return not (
        pytestconfig is not None
        and pytestconfig.getoption("--permissive-actuation", default=False)
    )


def _assert_refused_or_flagged(
    app, element_id: str, route: str, drive, pytestconfig=None
) -> None:
    """A disabled control is never *silently* driven: refused (strict) or flagged
    (permissive). Never honoured with no trace, which is the old bug."""
    if not _strict(pytestconfig):
        # Permissive: the gate MARKS and proceeds, so the assertion of record is
        # the marker — not that the actuation then succeeded. Those are different
        # claims, and on windows they routinely come apart: once the gate lets the
        # call through, UIA's own `Invoke`/`SetValue`/`Expand` may still throw
        # `ElementNotEnabledException` for the same disabled element, arriving as a
        # generic 500. That is windows' pre-existing behaviour and the baseline a
        # sweep must not disturb — so tolerate a downstream failure and still
        # require the marker, while insisting it is NOT the strict refusal (which
        # would mean the bridge and this test disagree about the mode).
        try:
            drive()
        except Exception as exc:  # noqa: BLE001 — re-asserted immediately below
            assert "element is disabled" not in str(exc), (
                f"permissive mode must not REFUSE the actuation — the gate is "
                f"supposed to mark it and proceed. Got the STRICT refusal, so "
                f"the bridge and this test disagree about the mode: {exc}"
            )
        _assert_marker_logged(app, element_id, route)
        return

    with pytest.raises(RuntimeError) as excinfo:
        drive()
    message = str(excinfo.value)
    assert "409" in message, (
        f"the refusal must be a 409: a 404 would send the driver into its "
        f"scroll-retry loop and report 'not rendered yet', the opposite "
        f"diagnosis for an element the bridge had already resolved. Got: {message}"
    )
    assert "element is disabled" in message, message
    assert element_id in message, (
        f"the refusal must name the element the test asked for: {message}"
    )
    assert route in message, (
        f"the refusal must name the ROUTE, or a sweep log cannot be triaged "
        f"per route: {message}"
    )


def test_a_disabled_button_is_refused_or_flagged(logged_in_app, pytestconfig):
    """`click` on a control the UI built disabled.

    Before the gate this went straight to `Invoke`/`Toggle`/`SelectionItem` and,
    failing those, to a physical `SendInput` click that strikes the pixel of a
    disabled button and acks 200 — the silent-honour case.
    """
    app = logged_in_app
    if not app.driver.is_windows():
        pytest.fail(
            "this suite drives the windows FlaUI bridge by construction; "
            "--app should have deselected it"
        )

    app.backups.navigate()
    app.driver.wait_for(CONFIRM_BUTTON)

    # Precondition, asserted rather than assumed: the friction bar builds
    # disabled. If the app ever stops disabling it, the premise of this test is
    # gone and we must fail loudly here rather than "pass" by exercising an
    # enabled control.
    assert app.driver.is_enabled(CONFIRM_BUTTON) is False, (
        f"premise broken: {CONFIRM_BUTTON} is declared IsEnabled=\"False\" "
        f"(Views/BackupsPage.xaml) and arms only when the typed text equals the "
        f"selected snapshot's id, but it reads enabled. "
        f"{app.driver.diagnose(CONFIRM_BUTTON, attrs=('enabled',))}"
    )

    _assert_refused_or_flagged(
        app,
        CONFIRM_BUTTON,
        "click",
        lambda: app.driver.click(CONFIRM_BUTTON),
        pytestconfig,
    )


def test_a_disabled_picker_is_refused_or_flagged(logged_in_app, pytestconfig):
    """`select` on a disabled ComboBox — the route whose ORDER convention 11
    settled.

    The gate fires after the structural check (is this even a ComboBox — a wrong
    id is a different bug class) and BEFORE the option-membership check, because a
    disabled picker's option list is routinely empty for the very reason it is
    disabled: membership-first reports *"'x' not found in 'y' (visible items: )"*
    for a control whose real story is that you cannot touch it at all.
    """
    app = logged_in_app
    app.backups.navigate()
    app.driver.wait_for(SOURCE_SELECT)

    assert app.driver.is_enabled(SOURCE_SELECT) is False, (
        f"premise broken: {SOURCE_SELECT} is declared IsEnabled=\"False\" and is "
        f"armed by a configured destination (Views/BackupsPage.xaml), and a fresh "
        f"account configures none, but it reads enabled. "
        f"{app.driver.diagnose(SOURCE_SELECT, attrs=('enabled',))}"
    )

    _assert_refused_or_flagged(
        app,
        SOURCE_SELECT,
        "select",
        lambda: app.driver.select(SOURCE_SELECT, "any-destination"),
        pytestconfig,
    )


def test_enabled_controls_are_untouched_by_the_gate(logged_in_app):
    """The gate is PRECISE: an enabled control is driven exactly as before.

    This is the regression that would matter most. The gate now sits on the hot
    path of every click/type/clear/select the windows harness issues, so an
    over-broad predicate — reading a stale value, treating an unreadable UIA
    property as disabled, or gating the read routes — would not fail one test, it
    would fail hundreds, in ways that read as product bugs.

    `clear` then `type` on an always-enabled field is the subject deliberately:
    both are gated, both reach the physical fallback on some controls, and a gate
    that broke ordinary typing would be far worse than the bug it fixes.
    """
    app = logged_in_app
    app.backups.navigate()
    app.driver.wait_for(CONFIRM_INPUT)

    assert app.driver.is_enabled(CONFIRM_INPUT) is True, (
        f"premise: the friction-bar input is always typeable — only the button "
        f"it arms is gated. {app.driver.diagnose(CONFIRM_INPUT, attrs=('enabled',))}"
    )

    app.driver.clear_and_type(CONFIRM_INPUT, "premise-check")
    assert app.driver.get_text(CONFIRM_INPUT) == "premise-check"


def _assert_marker_logged(app, element_id: str, route: str) -> None:
    """Assert the permissive-mode marker reached this run's captured BRIDGE
    stderr.

    windows' gate lives in the FlaUI bridge, so `app_stderr_text()` — the app's
    own log — never sees it; `bridge_stderr_text()` is the analogue of what
    linux/tui read. The run-scoped `--actuation-log` file is the sweep's harvest
    and is fed by the same one `ActuationGate.Check` call, so pinning either pins
    both. stderr is the one available without a flag, which is what makes this
    test meaningful in an ordinary `--app windows` run rather than only inside a
    sweep.
    """
    stderr = app.driver.bridge_stderr_text()
    assert DISABLED_MARKER in stderr, (
        f"permissive mode must leave a countable trace: no {DISABLED_MARKER!r} "
        f"marker in the bridge's stderr after driving a disabled {element_id}. "
        f"Without it, one run cannot enumerate the offenders and the staging "
        f"plan has no measurement. stderr tail: {stderr[-1200:]}"
    )
    assert f"{route} id={element_id}" in stderr, (
        f"the marker must name the route and the element, or a sweep's log "
        f"cannot be triaged per element. stderr tail: {stderr[-1200:]}"
    )
