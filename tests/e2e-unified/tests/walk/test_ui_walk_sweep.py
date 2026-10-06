"""Convention 17 layer (c) — the scheduled UI walk sweep.

Read `helpers/ui_walk.py`'s module docstring first: it owns the design, and in
particular *why this layer is deliberately thin*. The one-line version: breadth
belongs at tier_1 (layer (a)), continuous observation belongs on the existing
suite (layer (b)), and what only this layer can prove is that the walk vocabulary
reaches the **real binary** — a real focus ring, through a real key handler,
under the real registry and the real render.

**Gate:** `--walk-sweep`. Without it `conftest.py` prunes this directory outright
(the `tests/real_session/` and `tests/artifact/` treatment), because convention 17
says layer (c) is *"scheduled sweeps only, never inner-loop"* and an in-test skip
would still let a tier filter or a `just` recipe pull it in.

    pytest tests/walk/ --walk-sweep --app tui
    pytest tests/walk/ --walk-sweep --app linux

⚠ **Never add a sleep to this file.** Every step's ack *is* its barrier (the
agent sets `last_command_id` only after applying the command on its UI thread),
so every assertion below reads state that is already true when it is read. There
is no latency to wait out, which is what makes a walk this long safe to run on a
loaded box — the property convention 14 demands and the reason the sleep-ratchet
gate would reject one anyway.
"""

import pytest

from helpers import app_surface, ui_walk

pytestmark = pytest.mark.tier_3


def test_the_walk_vocabulary_carries_a_real_sweep_of_the_real_app(logged_in_app):
    """Walk every canonical page's focus ring, asserting invariants each step.

    Three things are asserted, and the order matters — the last one is what stops
    this test from being the very failure mode convention 17 exists to close.
    """
    app = logged_in_app
    if app.driver.is_ios():
        # `focus_move`/`switch_pane` are a DECLARED ABSENCE on iOS, not unbuilt
        # debt — UIKit exposes no public in-process API to trigger the focus
        # engine's Tab-equivalent traversal, and this app's shell has no
        # sidebar/content two-region split for `switch_pane` to cross in the
        # first place. A real refusal here would fail assertion (1) below on
        # every run, which is the wrong signal for a structural absence.
        app_surface.declared_absence(
            app.driver,
            capability="focus_move/switch_pane (convention 17 layer (c) walk "
            "vocabulary)",
            doc="docs/goal/architecture/apps/apple-e2e-automation.md § "
            "Declared platform absences",
        )
    report = ui_walk.WalkReport()
    walker = ui_walk.WalkDriver(app, report)

    for page in ui_walk.canonical_pages():
        try:
            app.driver.set_state({"nav": {"stack": [{"view": page}]}})
        except Exception as exc:
            # Navigation is fixture setup, not the subject. A page this app does
            # not mount is a parity question for that app's own area, and failing
            # the sweep on it would make the walk a nav-coverage test wearing a
            # walk's name.
            report.not_applicable[f"nav:{page}"] = 1
            print(f"[walk] skipped page {page!r}: {type(exc).__name__}: {exc}")
            continue
        walker.sweep_page(page)

    # (1) The invariant catalogue held on every frame the walk produced.
    assert not report.violations, (
        f"{len(report.violations)} invariant violation(s) over "
        f"{report.frames_read} frames on pages {report.pages_visited}:\n  "
        + "\n  ".join(report.violations)
    )

    # (2) The walk actually walked. Without this the sweep passes trivially the
    #     day the page list, the fixture or the nav patch quietly stops
    #     producing pages — green, fast, and measuring nothing.
    assert report.pages_visited, (
        "the sweep visited NO page — every canonical page failed to mount. That "
        "is a broken fixture reporting a clean walk, which is worse than a red."
    )

    # (3) The frames were real. This is the specific lesson layer (b) paid for:
    #     a probe that observed nothing across nine green tests read exactly like
    #     a probe that observed everything and found nothing.
    expected_steps = len(report.pages_visited) * (ui_walk.FOCUS_RING_STEPS + 3)
    assert report.steps == expected_steps, (
        f"walked {report.steps} steps, expected {expected_steps} for "
        f"{len(report.pages_visited)} pages — the sweep did not run the shape it "
        "reports."
    )
    assert not report.unobserved, (
        f"{len(report.unobserved)} of {report.steps} steps produced NO readable "
        f"frame: {report.unobserved[:10]}. A sweep whose frames cannot be read "
        "has asserted nothing — the invariant catalogue judged "
        f"{report.frames_read} frames, and a wholly unobserved run reports "
        "exactly like a clean one. n/a tallies: {}".format(report.not_applicable)
    )
    assert report.frames_read == report.steps, (
        f"walked {report.steps} steps but judged {report.frames_read} frames — "
        "the two must agree once nothing is unobserved."
    )
