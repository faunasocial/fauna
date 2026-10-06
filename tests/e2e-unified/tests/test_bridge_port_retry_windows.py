"""The FlaUI bridge retries a DIFFERENT port on a bind conflict instead of dying at
startup on its first collision.

**What this pins.** ``Program.cs`` drew its listening port with a single
``new Random().Next(18000, 19000)`` and called ``HttpListener.Start()`` exactly once —
no retry. Two bridges alive at once (the two-seat fixture launches an initiator seat and
a recipient seat back to back, each its own bridge process; a sibling Windows session's
bridge counts too) collide with odds ≈ (bridges alive)/1000, and the loser died with
``HttpListenerException (183)`` before it ever printed ``BRIDGE_PORT=``, which
``drivers/windows.py::_start_bridge`` surfaces as ``RuntimeError: Bridge didn't report
port`` / ``Bridge exited early`` — an unexplained setup ERROR on a column that did
nothing wrong.

**Measured 2026-09-21 on Windows.** One collision killed the recipient seat's launch in
``test_offline_share_two_seat.py`` mid-suite.
A follow-up 240-launch stress run (30 concurrent bridges × 8 trials, driven through
``WindowsBridgeDriver.start_bridge_only()``, each stopped through the driver's own
``teardown()``) reproduced it 3 times, all ``HttpListenerException (183)`` — the same
signature, at the rate the odds predict.

**Why a bind-conflict FAMILY, not the one measured code.** ``HttpListener.Start()``
against a port a *second* ``HttpListener`` already owns (the measured production
shape, two FlaUI bridges racing the same draw) throws ``ErrorCode 183``
(``ERROR_ALREADY_EXISTS``) — but against a port a *plain* socket owns (the shape
``web-bridge/server.py`` would collide in, since it draws from the identical
18000-19000 range on the same box) it throws a DIFFERENT code, ``32``
(``ERROR_SHARING_VIOLATION``), confirmed experimentally 2026-09-22 (a scratch
``HttpListener`` against a port held by a plain ``Socket`` vs. a port held by a second
``HttpListener``). Hard-coding ``== 183`` would silently stop retrying on the 32 case.

**Why this is not a "needs a full e2e run" check.** The retry policy is pure control
flow over an injected draw + an injected bind attempt — no real listener, no port, no
app — so ``SelfTest.PortRetryChecks()`` (``--self-test-port-retry``, mirroring
``--self-test-scroll-policy`` / ``--self-test-handle-lifetime`` /
``--self-test-actuation-gate``) pins it directly: a ``[taken, free]`` draw sequence
lands on the free port and never retries the SAME port twice; both native error codes
in the conflict family are retried; a non-conflict exception propagates instead of
being swallowed; and exhausting the bound raises rather than retrying forever.

Goal doc: `docs/goal/architecture/e2e-conventions.md` § convention 10 — the "any
harness child that picks its own listening port" sentence added in the same commit as
this fix (no convention ruled the FlaUI bridge's own port choice before it).

tier_2: a real harness binary (the bridge) evaluating pure control flow, no app.
"""

from __future__ import annotations

import subprocess

import pytest

from drivers.windows import _BRIDGE_EXE, _ensure_bridge_built

pytestmark = [pytest.mark.tier_2, pytest.mark.windows]


def test_bridge_port_choice_retries_a_different_port_on_a_bind_conflict() -> None:
    """The port-picker self-test must be green: it fails for the right reason
    (a compile error naming the missing `PortPicker` type) until the fix lands."""
    _ensure_bridge_built()
    proc = subprocess.run(
        [str(_BRIDGE_EXE), "--self-test-port-retry"],
        capture_output=True, text=True, timeout=60,
    )
    # The self-test names each failure on its own line; surface the whole
    # transcript so the failure diagnoses itself (convention 6).
    assert proc.returncode == 0, (
        f"bridge port-retry self-test reported {proc.returncode} failure(s).\n"
        f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
    )
