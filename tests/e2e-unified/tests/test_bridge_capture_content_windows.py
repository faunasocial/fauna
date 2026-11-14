"""The FlaUI bridge's window-capture content guard — pinned with no window and no app.

**What this pins.** `Actions.CaptureWindow` (the store-listing screenshots' path,
`docs/goal/architecture/installers/windows.md` § Store distribution, the screenshots
paragraph) refuses a capture that shows nothing the app painted. It used to ask that of
the WHOLE frame, and a session with no attached desktop (`query session` shows it `Disc`)
defeats that: `PrintWindow` still draws the non-client title bar while the client area
underneath stays flat white. On Windows 2026-11-12 both listing shots came back exactly so
and both tests passed, writing two blank images over the committed pair.

`CaptureContent.RegionIsUniform` asks it of the client area alone, and
`SelfTest.CaptureContentChecks()` (driven by `--self-test-capture-content`, mirroring
`--self-test-actuation-gate`) grades it on synthetic frames: the disconnected-session
frame is refused, one painted pixel inside the client area keeps a capture, a region past
the frame is clipped, and an empty region shows nothing.

Goal doc: `docs/goal/architecture/installers/windows.md` § Store distribution (the
screenshots paragraph: the harness captures the app's own window, and a shot shows only
what the app does for real).

tier_2: a real harness binary (the bridge) evaluating pure functions, no app.
"""

from __future__ import annotations

import subprocess

import pytest

from drivers.windows import _BRIDGE_EXE, _ensure_bridge_built

pytestmark = [pytest.mark.tier_2, pytest.mark.windows]


def test_bridge_capture_refuses_a_flat_client_area_under_a_painted_title_bar() -> None:
    """A capture is kept only when its client area holds more than one colour."""
    _ensure_bridge_built()
    proc = subprocess.run(
        [str(_BRIDGE_EXE), "--self-test-capture-content"],
        capture_output=True, text=True, timeout=60,
    )
    # The self-test names each failure on its own line; surface the whole
    # transcript so the failure diagnoses itself (convention 6).
    assert proc.returncode == 0, (
        f"bridge capture-content self-test reported {proc.returncode} failure(s).\n"
        f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
    )
