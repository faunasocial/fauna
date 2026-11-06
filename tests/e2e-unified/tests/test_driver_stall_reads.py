"""A held UI thread is never read as an absent or a disabled element — the
driver's half.

`docs/goal/architecture/e2e-conventions.md` convention 11 (the stall stack): an
app op that fails on a held UI thread carries the stall verdict, whose first line
is ``UI-THREAD STALL``. The windows bridge lets that failure through on every read
that answers for an element (`flaui-bridge/StallWatch.cs::UiaSwallow`; the forced
stall is `test_ui_thread_stall_stack_windows.py`). This pins that the shared HTTP
driver does not swallow it again: ``count``/``is_visible`` raise on any bridge
error, and ``is_enabled`` — whose except-False otherwise answers "not enabled" for
any failure — raises on the verdict alone.

tier_1: pure Python, no nest binary, no client driver, no bridge process.
"""
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.http_bridge import UI_THREAD_STALL_HEADER, HttpBridgeDriver  # noqa: E402

STALL_FAILURE = (
    "Bridge error (500): System.TimeoutException: UIA Timeout\n"
    f"{UI_THREAD_STALL_HEADER} (the windows app, judged from outside by the FlaUI bridge "
    "after 2s in GET /element/enabled): the app ran +12 ms of CPU ..."
)


def _driver_failing_with(message: str):
    """A bare ``HttpBridgeDriver`` whose every bridge read fails with
    ``RuntimeError(message)`` — the shape ``_get`` raises for a 500."""

    def failing_get(path, params=None):
        raise RuntimeError(message)

    d = HttpBridgeDriver.__new__(HttpBridgeDriver)
    d._get = failing_get
    return d


def test_is_enabled_lets_a_stall_verdict_through():
    d = _driver_failing_with(STALL_FAILURE)
    with pytest.raises(RuntimeError, match=UI_THREAD_STALL_HEADER):
        d.is_enabled("compose-send-button")
    with pytest.raises(RuntimeError, match=UI_THREAD_STALL_HEADER):
        d.is_disabled("compose-send-button")


def test_is_enabled_still_reads_any_other_failure_as_not_enabled():
    d = _driver_failing_with("Bridge error (500): System.InvalidOperationException: other")
    assert d.is_enabled("compose-send-button") is False


@pytest.mark.parametrize("read", ["count", "is_visible"])
def test_count_and_visibility_let_a_stall_verdict_through(read):
    d = _driver_failing_with(STALL_FAILURE)
    d._scope_wire = lambda scope: None
    with pytest.raises(RuntimeError, match=UI_THREAD_STALL_HEADER):
        getattr(d, read)("dm-message-text")
