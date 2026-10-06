"""The FlaUI bridge keeps a usable handle to the app it launched, for the whole session.

**What this pins.** ``SessionManager`` must be able to kill the app THROUGH the handle it
holds, at any point in a session. The bug was that it could not: ``Launch`` handed
FlaUI the bridge's own ``Process`` object, and ``Application.GetMainWindow`` — which every
element find goes through — calls ``Dispose()`` on the process it holds whenever the main
window handle is missing::

    at System.ComponentModel.Component.Dispose()
    at FlaUI.Core.Application.<WaitWhileMainHandleIsMissing>b__33_0()
    at FlaUI.Core.Tools.Retry.WhileTrue(...)
    at FlaUI.Core.Application.WaitWhileMainHandleIsMissing(...)
    at FlaUI.Core.Application.GetMainWindow(AutomationBase, waitTimeout)

One object, two owners, and the owner that did not expect it lost its only view of its own
app: every later ``Refresh()``/``Id``/``HasExited`` threw ``InvalidOperationException: No
process is associated with this object``. Two costs, both real:

1. ``Quit``'s kill loop read that exception as "already gone" and reported the session
   closed while the app ran on — the harness leak the data-dir sweep was compensating
   for (it fired on EVERY relaunch, 3/3 in each run, and 5/5 at one module boundary).
2. Mid-session it broke **every subsequent find** for the rest of the run, because the
   ``Application`` itself is unusable once its process handle is gone. That cost real
   feature coverage — 2-3 of ``test_backups.py``'s 15 cases in every run — on a page with
   nothing to do with process lifecycle, with an onset that moved between runs (test 8 in
   one, test 14 in another) purely because it depends on when a find catches the main
   window transiently missing.

**Why this is not a "needs a human"/"needs a full e2e run" check.** The disposing door is
"the main window handle is missing", so ANY windowless process is permanently in exactly
that state. The C# side (``flaui-bridge/SelfTest.cs``) drives the real ``SessionManager``
with ``ping`` standing in for the app, and asserts on latency-independent state: the
tracked handle answers after a find, and after ``Quit`` an INDEPENDENT witness handle says
the process actually exited. No GUI, no nest, no app build, seconds not minutes — and it
was red before the fix (3 failures, exit 3) and green after.

The stand-in launches with no data dir on purpose: ``Quit``'s data-dir sweep then has
nothing to scan, so its verdict is purely the handle's. The sweep stays the standing
detector for the real runs — this test pins the layer underneath it, so the sweep's
silence means "nothing leaked" rather than "the net caught it again".

Goal doc: `docs/goal/architecture/e2e-conventions.md` § point 10 (module-boundary cold
relaunch contract — the bridge must own its app's lifecycle).

tier_2: a real harness binary (the bridge) driving a stubbed app.
"""

from __future__ import annotations

import subprocess

import pytest

from drivers.windows import _BRIDGE_EXE, _ensure_bridge_built

pytestmark = [pytest.mark.tier_2, pytest.mark.windows]


def test_bridge_app_handle_survives_a_find_and_kills_through_it() -> None:
    """A find must not cost the bridge the handle to its own app."""
    _ensure_bridge_built()
    proc = subprocess.run(
        [str(_BRIDGE_EXE), "--self-test-handle-lifetime"],
        capture_output=True, text=True, timeout=180,
    )
    # The self-test names each failure on its own line; surface the whole
    # transcript so the failure diagnoses itself (convention 6).
    assert proc.returncode == 0, (
        f"bridge handle-lifetime self-test reported {proc.returncode} failure(s).\n"
        f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
    )
