"""The FlaUI bridge's stall verdict for the windows app — convention 11's split,
pinned with no app, no window and no stall.

The windows app is a C# process that beats no `fauna_e2e_agent` heartbeat, so the
bridge decides whether a stalled op is the app's own (the app's CPU moved and its
UI thread is not waiting for a core) or the box's (`flaui-bridge/StallWatch.cs`,
`StallVerdict.Decide`), and only the app-side verdict takes the UI thread's stack.
`SelfTest.StallVerdictChecks()` (driven by `--self-test-stall-verdict`, mirroring
`--self-test-actuation-gate`) grades every corner: app-side, each runnable state
read as the box, an app that ran no CPU, each missing reading as UNMEASURED, the
faked starved reading, which routes are watched, what counts as a UIA timeout, and
that the reply budget lands under the driver's own.

The forced stall on a real app is `test_ui_thread_stall_stack_windows.py`.

Goal doc: `docs/goal/architecture/e2e-conventions.md` § convention 11 (the stall
stack, the windows app's leg).

tier_2: a real harness binary (the bridge) evaluating pure functions, no app.
"""

from __future__ import annotations

import subprocess

import pytest

from drivers.windows import _BRIDGE_EXE, _ensure_bridge_built

pytestmark = [pytest.mark.tier_2, pytest.mark.windows]


def test_bridge_stall_verdict_corners_are_pinned() -> None:
    """Only an app that ran, with a UI thread not waiting for a core, is judged
    app-side; a runnable UI thread or an idle app is the box; a missing reading
    is UNMEASURED."""
    _ensure_bridge_built()
    proc = subprocess.run(
        [str(_BRIDGE_EXE), "--self-test-stall-verdict"],
        capture_output=True, text=True, timeout=60,
    )
    # Each check names itself on its own line; the whole transcript is the
    # failure message (convention 6).
    assert proc.returncode == 0, (
        f"stall verdict self-test failed (exit {proc.returncode}):\n"
        f"{proc.stdout}\n{proc.stderr}"
    )
    assert "all checks passed" in proc.stdout, proc.stdout
