"""The admin forms' draft-validity check, one shape for every test that types a
malformed draft (`docs/goal/behavior/admin.md` § Errors & edge cases → *A form's
submit is live only for a draft the nest would accept*; `docs/goal/ui/README.md`
§ Offer only what is possible, rule 4 — the form case).

An app that has built the surface (`admin-form-draft-validity` in
`tests/e2e-unified/surfaces.toml`) paints the submit disabled while the draft is
malformed — its agent refuses a press on a disabled control, so the test reads
the paint and never presses. An app still in its trickle-down lift validates
only at the press, so the test presses and reads the refusal on the page's error
line. Either way the caller then asserts that nothing reached the nest: that
assertion, not this helper, is the causal check.

The reason's sentence on a built app is pinned by the shared predicate's unit
tests and tui's gate-map test; this helper does not re-read it off the paint.
"""
from __future__ import annotations

import time

from helpers.app_surface import app_name, surface_built

SURFACE = "admin-form-draft-validity"


def draft_validity_built(app) -> bool:
    """Whether the app under test paints its admin submits from the draft."""
    return surface_built(SURFACE, app_name(app.driver))


def refused_before_or_at_the_press(
    app,
    *,
    submit_id: str,
    press,
    reason: str,
    error_id: str = "error-message",
    scope: str | None = None,
    timeout: float = 15.0,
) -> None:
    """Assert the malformed draft is refused: painted blocked on a built app,
    refused with ``reason`` on ``error_id`` at the press elsewhere.

    ``press`` is the action that presses ``submit_id`` (the lagging path only).
    The polls are generous deadlines over a local repaint, never settle sleeps
    (convention 14).
    """
    if draft_validity_built(app):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and app.driver.is_enabled(submit_id, scope=scope):
            time.sleep(0.3)
        assert not app.driver.is_enabled(submit_id, scope=scope), (
            f"{submit_id} is live on a malformed draft; the form case paints it "
            f"blocked with the reason {reason!r}. error: {app.error_text()!r}"
        )
        return
    press()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and reason not in (app.driver.get_text(error_id) or ""):
        time.sleep(0.3)
    got = app.driver.get_text(error_id) or ""
    assert reason in got, f"{reason!r} not surfaced on {error_id}; got {got!r}"
