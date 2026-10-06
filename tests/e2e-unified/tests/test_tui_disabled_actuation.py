"""tier_3 e2e: the tui in-process agent must not drive a control the UI disabled.

tui's `automation::perform` gated **two** of its five actuation routes.
`ElementKind::Click` and `ElementKind::DoubleClick` consulted `entry.enabled`;
`Type`/`Clear` checked only `entry.field` and `Select` only `entry.options`, so
a disabled field was typed into, a disabled picker was set, and both replied
`{"ok": true}`. That is `e2e-conventions.md` convention 11 **one layer down** —
not a *dropped* command but an **illegal one silently honoured** — and the
convention names this exact shape: *"Gating click alone is not compliance:
typing into a disabled field is the same illegal act, and a half-applied gate
leaves the hole open for the next session to rediscover."*

The refusals it *did* make were a bare `{"error": "disabled"}` carrying no
`status`, so they rode `fauna_e2e_agent::element`'s default **404** →
`drivers/http_bridge.py` → `LookupError` → `_post_with_scroll`'s retry loop, and
a test clicking a disabled control scrolled three times and died reading *"not
rendered yet"* — the opposite diagnosis, for an element the agent had just
resolved. Both halves were fixed 2026-08-21: all five routes now go through the
shared `fauna_e2e_agent::gate_actuation`, which linux hosts too.

**tui is NOT staging.** Unlike linux — which landed the same gate in the
opt-in-strict posture convention 11 prescribes for an app that has never refused
(`test_linux_disabled_actuation.py`) — tui has refused a disabled `click` since
its automation surface existed, so `default_strict = true` and refusal is the
default here. `--permissive-actuation` is the way OUT of refusal, apple's
posture, and it survives as the measuring instrument for the next broad change
rather than as staging scaffolding to delete.

**This file is the sweep's known-positive control.** It drives disabled controls
on purpose, so its markers MUST appear in any permissive `--app tui` sweep's log.
A zero-violation sweep whose log lacks `id=restore-source-select` means the
detector never ran, not that nothing violated — apple's first iOS sweep reported
a whole target clean while 567 of its tests had silently skipped, and the probe's
absent marker is the only thing that caught it.

**The subjects** all paint on the Backups page of a FRESH account, with no
seeding, no snapshot and no destination — `backups.rs::restore_elements` is
deliberately outside the destinations `else` because "the restore half's local
path works with zero destinations configured":

* `restore-source-select` — `enabled: !sources_empty`, and a fresh account has
  configured no destinations. A disabled **select**, the most interesting of the
  three newly-gated routes.
* `restore-confirm-button` — `st.restore_armed()`, false until the typed text
  equals the selected snapshot's id, and there are no snapshots. The same
  friction bar linux's probe uses, which makes the two files true twins.
* `restore-confirm-input` — a plain `Role::Input` beside them, always enabled:
  the precision control proving the gate did not over-refuse.

tier_3: real `fauna-nest` binary via `logged_in_app`; tui only (it drives the tui
in-process agent's own HTTP surface).
"""

from __future__ import annotations

import os

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

# tests/e2e-unified/ui.yaml § backups.
SOURCE_SELECT = "restore-source-select"
CONFIRM_BUTTON = "restore-confirm-button"
CONFIRM_INPUT = "restore-confirm-input"

# The marker `fauna_e2e_agent::gate_actuation` logs for every violation. Shared
# verbatim with linux's and apple's, so one grep spans a cross-app sweep.
DISABLED_MARKER = "DISABLED-ACTUATION"


def _permissive(pytestconfig) -> bool:
    """Is refusal switched OFF for this run?

    **Must consult the pytest option, not just `os.environ`.** This helper read
    only the environment until 2026-08-29, on the reasoning that the app's own
    branch is the truth and "the option only sets it". The second half is false
    for tui: `conftest._apply_actuation_mode_env` writes
    `FAUNA_E2E_PERMISSIVE_ACTUATION` into the **per-launch app environment dict**,
    never into the pytest process's own environment, so under
    `--permissive-actuation` the app branched permissive while this returned
    False. The two disagreed, these tests took the strict branch against a
    permissive app, and the known-positive control could not pass **in the only
    mode a sweep ever runs in** — which is why task F was blocked for three
    attempts.

    linux's `_strict()` twin reads the environment correctly, and the difference
    is real rather than an inconsistency to unify away: its
    `FAUNA_E2E_STRICT_ACTUATION` is an opt-IN a human exports into the shell, so
    it genuinely is in `os.environ`. tui's flag has exactly one source — this
    option — so the option is what must be read. The env is still honoured so a
    hand-exported run behaves the same.
    """
    if pytestconfig is not None and pytestconfig.getoption(
        "--permissive-actuation", default=False
    ):
        return True
    return bool(os.environ.get("FAUNA_E2E_PERMISSIVE_ACTUATION"))


def _assert_refused_or_flagged(
    app, element_id: str, route: str, drive, pytestconfig=None
) -> None:
    """A disabled control is never *silently* driven: refused (the default) or
    flagged (permissive). Never honoured with no trace, which is the old bug."""
    if _permissive(pytestconfig):
        # In permissive mode the gate MARKS and proceeds, so the assertion of
        # record is the marker — not that the actuation then succeeded. Those
        # are different claims, and for `select` they come apart: the gate lets
        # the call through, and the select's own option-list check then refuses
        # `SelectOptionNotOffered`, because a picker painted disabled offers no
        # options at all ("this frame painted []"). Strict mode never sees that,
        # since the disabled refusal fires first. So tolerate a downstream
        # refusal here and still require the marker.
        try:
            drive()
        except Exception as exc:  # noqa: BLE001 — re-asserted immediately below
            assert "element is disabled" not in str(exc), (
                f"permissive mode must not REFUSE the actuation — the gate is "
                f"supposed to mark it and proceed. Got the STRICT refusal, so "
                f"the app and this test disagree about the mode: {exc}"
            )
        _assert_marker_logged(app, element_id, route)
        return

    with pytest.raises(RuntimeError) as excinfo:
        drive()
    message = str(excinfo.value)
    assert "409" in message, (
        f"the refusal must be a 409: a 404 would send the driver into its "
        f"scroll-retry loop and report 'not rendered yet', the opposite "
        f"diagnosis for an element it had already resolved. Got: {message}"
    )
    assert "element is disabled" in message, message
    assert element_id in message, (
        f"the refusal must name the element the test asked for: {message}"
    )
    assert route in message, (
        f"the refusal must name the ROUTE, or a sweep log cannot be triaged "
        f"per route: {message}"
    )


def test_a_disabled_picker_is_refused_or_flagged(logged_in_app, pytestconfig):
    """`select` — one of the three routes that were ungated until 2026-08-21.

    Before the fix this call returned `{"ok": true}` after writing the value
    through, so a test could drive the picker to a state no keystroke can reach
    (tui's keyboard path cycles the option list, and a disabled element's
    `Enter` is inert) — convention 8's "API-only mutation path" wearing a UI
    costume.
    """
    app = logged_in_app
    if not app.driver.is_tui():
        pytest.fail(
            "this suite drives the tui in-process agent by construction; "
            "--app should have deselected it"
        )

    app.backups.navigate()
    app.driver.wait_for(SOURCE_SELECT)

    # Precondition, asserted rather than assumed: with no destination configured
    # the picker paints disabled. If the app ever stops disabling it the premise
    # of this test is gone, and we must fail loudly here rather than "pass" by
    # exercising an enabled control.
    assert app.driver.is_enabled(SOURCE_SELECT) is False, (
        f"premise broken: {SOURCE_SELECT} paints `enabled: !sources_empty` "
        f"(backups.rs::restore_elements) and a fresh account configures no "
        f"destinations, but it reads enabled. "
        f"{app.driver.diagnose(SOURCE_SELECT, attrs=('enabled',))}"
    )

    _assert_refused_or_flagged(
        app,
        SOURCE_SELECT,
        "select",
        lambda: app.driver.select(SOURCE_SELECT, "any-destination"),
        pytestconfig,
    )


def test_a_disabled_button_is_refused_or_flagged(logged_in_app, pytestconfig):
    """`click` — gated all along, but its refusal shape changed.

    tui refused this before 2026-08-21 too, with a bare `{"error": "disabled"}`
    that carried no status and so rode the default 404 into the driver's
    scroll-retry loop. The assertion that matters here is therefore the 409 and
    the named message, not the mere fact of refusal.
    """
    app = logged_in_app
    app.backups.navigate()
    app.driver.wait_for(CONFIRM_BUTTON)

    assert app.driver.is_enabled(CONFIRM_BUTTON) is False, (
        f"premise broken: {CONFIRM_BUTTON} is armed only when the typed text "
        f"equals the selected snapshot's id (backups.rs::restore_armed) and a "
        f"fresh account has no snapshots, but it reads enabled. "
        f"{app.driver.diagnose(CONFIRM_BUTTON, attrs=('enabled',))}"
    )

    _assert_refused_or_flagged(
        app,
        CONFIRM_BUTTON,
        "click",
        lambda: app.driver.click(CONFIRM_BUTTON),
        pytestconfig,
    )


def test_enabled_controls_are_untouched_by_the_gate(logged_in_app):
    """The gate is PRECISE: an enabled control is driven exactly as before.

    This is the regression that would matter most. The gate sits on the hot path
    of every click/type/clear/select the tui harness issues, so an over-broad
    predicate — reading a stale `enabled`, or gating the read routes too — would
    not fail one test, it would fail hundreds, in ways that read as product bugs.

    The `type` route is the subject deliberately: it is one of the three that
    were ungated, so it is the one whose new gate is least exercised elsewhere,
    and a gate that broke ordinary typing would be far worse than the bug it
    fixes.
    """
    app = logged_in_app
    app.backups.navigate()
    app.driver.wait_for(CONFIRM_INPUT)

    assert app.driver.is_enabled(CONFIRM_INPUT) is True, (
        f"premise: the friction-bar input is always typeable — only the button "
        f"it arms is gated. {app.driver.diagnose(CONFIRM_INPUT, attrs=('enabled',))}"
    )

    # Drives `clear` then `type`, both newly gated routes, on an enabled
    # control. The read-back is the proof the keystrokes actually landed rather
    # than being swallowed by an over-broad refusal.
    app.driver.clear_and_type(CONFIRM_INPUT, "premise-check")
    assert app.driver.get_text(CONFIRM_INPUT) == "premise-check"


def _assert_marker_logged(app, element_id: str, route: str) -> None:
    """Assert the permissive-mode marker reached this launch's captured stderr.

    `app.err` is the launch-scoped witness (`drivers/tui.py::app_stderr_text`);
    the run-scoped `--actuation-log` file is the sweep's harvest, and it is fed
    by the same one `gate_actuation` call, so pinning either pins both. stderr is
    the one available without a flag, which is what makes this test meaningful in
    an ordinary `--app tui` run rather than only inside a sweep.
    """
    stderr = app.driver.app_stderr_text()
    assert DISABLED_MARKER in stderr, (
        f"permissive mode must leave a countable trace: no {DISABLED_MARKER!r} "
        f"marker in the app's stderr after driving a disabled {element_id}. "
        f"Without it, one run cannot enumerate the offenders and the sweep has "
        f"no measurement. stderr tail: {stderr[-1200:]}"
    )
    assert f"{route} id={element_id}" in stderr, (
        f"the marker must name the route and the element, or a sweep's log "
        f"cannot be triaged per element. stderr tail: {stderr[-1200:]}"
    )
