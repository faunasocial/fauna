r"""tier_1: the android driver reads the device clipboard through the bridge's
``GET /clipboard/text``, the windows / linux / apple contract.

The consumer is the copy journeys (outcome 13, ``helpers/recovery_restore.py::
copy_the_shown_kit``), which compare the copied payload against the shown kit.
Only the on-device ``ClipboardManager`` read needs a device; the driver's verb
(text through, ``None`` when empty, a refusal raises) is pinned here against a
stubbed bridge reply. No ``android`` app marker, deliberately: no device is needed.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1


def _driver_replying(reply: dict) -> AndroidBridgeDriver:
    driver = AndroidBridgeDriver.__new__(AndroidBridgeDriver)
    calls: list[str] = []
    driver._get = lambda path, params=None: (calls.append(path), reply)[1]  # type: ignore[method-assign]
    driver._calls = calls  # type: ignore[attr-defined]
    return driver


def test_clipboard_text_passes_through():
    driver = _driver_replying({"text": "fauna://recovery?k=abc"})
    assert driver.get_clipboard_text() == "fauna://recovery?k=abc"
    assert driver._calls == ["/clipboard/text"]  # type: ignore[attr-defined]


def test_empty_clipboard_is_none():
    assert _driver_replying({"text": None}).get_clipboard_text() is None


def test_bridge_refusal_raises_not_reads_empty():
    with pytest.raises(RuntimeError, match="android clipboard read refused"):
        _driver_replying({"error": "no focus"}).get_clipboard_text()
