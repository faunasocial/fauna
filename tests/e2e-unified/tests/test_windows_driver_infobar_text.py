"""tier_1: the windows driver's `get_text` drops the InfoBar's severity-icon chrome.

WinUI's InfoBar aggregates its severity glyph's accessible name ("Error icon") into
the text UIA reads off the message element, so `get_text("error-message")` came back
as `"Error icon <message>"` on windows and as `"<message>"` on every other app. A test
comparing the shown error to the catalogued string could therefore never hold on
windows — measured on `test_device_removal_refusal.py`, whose refusal painted with
exactly the right words and was still graded absent. The strip lives in the driver
(one place) rather than in each actions class, which is how the gap sat unfixed in
five of six copies of the same helper.

Pure state: the base `get_text` is replaced with a canned read and the driver is built
without launching anything.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers.http_bridge import HttpBridgeDriver  # noqa: E402
from drivers.windows import WindowsBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

MESSAGE = "This is the device you're using, so it wasn't removed."


def _reading(monkeypatch, raw):
    monkeypatch.setattr(
        HttpBridgeDriver, "get_text", lambda self, element_id, index=0, *, scope=None: raw
    )
    return object.__new__(WindowsBridgeDriver)


@pytest.mark.parametrize("severity", ["Error", "Warning", "Informational", "Success"])
def test_the_infobar_severity_icon_is_not_part_of_the_message(monkeypatch, severity):
    driver = _reading(monkeypatch, f"{severity} icon {MESSAGE}")
    assert driver.get_text("error-message") == MESSAGE


def test_a_text_without_the_icon_comes_back_untouched(monkeypatch):
    assert _reading(monkeypatch, MESSAGE).get_text("error-message") == MESSAGE


def test_only_a_leading_icon_name_is_dropped(monkeypatch):
    # An "Error icon" named inside real text is content, not chrome.
    raw = f"{MESSAGE} (the Error icon was shown)"
    assert _reading(monkeypatch, raw).get_text("error-message") == raw


def test_an_empty_read_stays_empty(monkeypatch):
    assert _reading(monkeypatch, "").get_text("error-message") == ""
