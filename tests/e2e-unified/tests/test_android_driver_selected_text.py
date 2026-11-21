r"""tier_1: the android driver reads text selection through the shared
``HttpBridgeDriver`` read (``_serves_select_text``) over the bridge's
``POST /element/select-text`` and ``POST /page/selection``
(``ElementOps.selectText`` / ``selectedText``) — the web / macos / linux / ios
contract: the selection's text through, ``""`` when none stands, a bridge
refusal raised (never read as "nothing selected"), and a long-press whose
toolbar came up but whose copy wrote nothing read True with its ``why`` kept for
``diagnose``.

The consumers are the four outcomes of ``copy-what-you-read``
(``test_read_only_text_is_selectable.py``). Only the on-device gesture and copy
need a device; the driver's verbs are pinned here against a stubbed bridge. No
``android`` app marker, deliberately: no device is needed.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1


def _driver(replies: dict, *, refusal: Exception | None = None, shown: str = "") -> AndroidBridgeDriver:
    driver = AndroidBridgeDriver.__new__(AndroidBridgeDriver)
    calls: list[str] = []

    def post(path, data=None, timeout=None):
        calls.append(path)
        if refusal is not None:
            raise refusal
        return replies[path]

    driver._post = post  # type: ignore[method-assign]
    driver._scope_wire = lambda scope: None  # type: ignore[method-assign]
    driver.get_text = lambda element_id, index=0, scope=None: shown  # type: ignore[method-assign]
    driver._calls = calls  # type: ignore[attr-defined]
    return driver


def test_android_serves_the_shared_select_text_read():
    assert AndroidBridgeDriver._serves_select_text is True


def test_selection_text_passes_through():
    driver = _driver({"/page/selection": {"selected": "selecting this text"}})
    assert driver.selected_text() == "selecting this text"
    assert driver._calls == ["/page/selection"]  # type: ignore[attr-defined]


def test_no_selection_is_empty_not_none():
    # None is the base's "this driver cannot read a selection"; android can.
    assert _driver({"/page/selection": {"selected": None}}).selected_text() == ""


def test_bridge_refusal_raises_not_reads_empty():
    refused = RuntimeError("Bridge error (500): the copy key (Ctrl+C) wrote nothing")
    with pytest.raises(RuntimeError, match="wrote nothing"):
        _driver({}, refusal=refused).selected_text()


def test_a_long_press_selection_inside_the_text_reads_selectable():
    driver = _driver(
        {"/element/select-text": {"selected": "selecting", "copy_offered": False}},
        shown="selecting this text opens nothing",
    )
    assert driver.is_text_selectable("feed-post-text") is True
    assert driver._calls == ["/element/select-text"]  # type: ignore[attr-defined]


def test_no_toolbar_reads_not_selectable():
    driver = _driver({"/element/select-text": {
        "selected": "", "copy_offered": False, "why": "no text-selection toolbar with Copy came up"}})
    assert driver.is_text_selectable("feed-tab") is False


def test_toolbar_without_a_copy_reads_selectable_and_keeps_why():
    why = "the selection toolbar came up, but the copy key (Ctrl+C) wrote nothing to the clipboard"
    driver = _driver({"/element/select-text": {"selected": "", "copy_offered": True, "why": why}})
    assert driver.is_text_selectable("error-message") is True
    assert driver._last_select_why == why  # type: ignore[attr-defined]
