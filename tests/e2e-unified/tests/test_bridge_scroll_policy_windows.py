"""The FlaUI bridge's scroll-sweep step policy and blind-scroll container choice
are pure geometry/selection logic, pinned without any real UIA element.

**What this pins.** Two problems left standing (which
fixed `ScrollIntoView`'s sweep bound but deliberately left its cost and the blind
fallback's container choice alone, since both change behaviour for EVERY windows
e2e test):

1. **Sweep step count.** `Actions.cs::ScrollIntoView`'s sweep advanced
   `SetScrollPercent` by half the container's own viewport per step (consecutive
   viewports overlapping 50%) so no element could fall *between* two scroll
   positions. That overlap is over-cautious for DISCOVERY: UIA's `IsOffscreen`
   flips false as soon as *any* part of an element is onscreen, and the real
   percent-to-offset mapping (`ScrollPolicy.ViewportStart` — percent walks the
   *scrollable range*, content minus viewport, not the content itself) guarantees
   a non-negative overlap even at a full 100%-of-viewport step. So the fix is a
   constant, not a redesign — but "no element can fall through" needs a proof,
   not a feeling, hence the geometry checks below.
2. **Blind scroll's container choice.** `Actions.cs::Scroll(direction)` picked the
   *first* tree descendant advertising a vertical `ScrollPattern`. On any settings
   sub-page that is the 25-item settings navigation rail (earlier in the UIA tree
   than the page content), not the content itself — so `wait_for`'s post-sweep
   blind-scroll retries have been silently scrolling the nav rail on every
   settings page. The fix is to prefer the WIDEST (largest bounding-rect area)
   vertical scroller, which is structurally the content frame, never the narrow
   rail — independent of tree order.

**Why this is not a "needs a full e2e run" check.** Both problems are pure
arithmetic/selection given a viewport-size percentage and a set of bounding-rect
areas — no FlaUI element, no app, no window needed. `ScrollPolicy.cs` factors
that arithmetic out of `Actions.cs` so `SelfTest.ScrollPolicyChecks()` (driven by
`--self-test-scroll-policy`, mirroring `--self-test-handle-lifetime`) can pin it
directly: a container whose viewport-derived step does not divide 100 evenly
still reaches exactly 100% (the regression, guarded again here); an element
in the sweep's final slice is found; a thin element straddling the tightest
overlap between two consecutive steps is still found (the geometry proof for
point 1); and the widest of several synthetic scrollable candidates wins
regardless of which one the UIA tree would enumerate first (the fix for point 2).
The step-count budget check is the one that is RED against the pre-fix
`SweepStepViewportFraction = 0.5` and green after `= 1.0` — the other checks hold
on both, proving the constant change costs nothing in coverage.

Goal doc: no goal doc owns the FlaUI bridge's internals (`Actions.cs`'s own doc
comments are its spec); the wait-budget rule this sweep must fit inside is
convention 14, owned by
`docs/goal/architecture/e2e-latency-independent-assertions.md`.

tier_2: a real harness binary (the bridge) evaluating pure functions, no app.
"""

from __future__ import annotations

import subprocess

import pytest

from drivers.windows import _BRIDGE_EXE, _ensure_bridge_built

pytestmark = [pytest.mark.tier_2, pytest.mark.windows]


def test_bridge_scroll_sweep_step_policy_is_gap_free_and_within_budget() -> None:
    """The sweep step plan must stay cheap (budget check, red pre-fix) while
    never leaving a gap an element could fall through (geometry checks)."""
    _ensure_bridge_built()
    proc = subprocess.run(
        [str(_BRIDGE_EXE), "--self-test-scroll-policy"],
        capture_output=True, text=True, timeout=60,
    )
    # The self-test names each failure on its own line; surface the whole
    # transcript so the failure diagnoses itself (convention 6).
    assert proc.returncode == 0, (
        f"bridge scroll-policy self-test reported {proc.returncode} failure(s).\n"
        f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
    )
