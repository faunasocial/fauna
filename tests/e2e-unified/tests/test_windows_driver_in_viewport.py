"""tier_1: the windows driver's `in_viewport` reads the bridge's three answers.

The FlaUI bridge's `in-viewport` attribute answers `true`, `false`, or
`undecided: <why>`: WinUI clips each UIA rect to its ScrollViewer, and a read that
never scrolls cannot always prove a rect is the whole element, so a reading that
may be clipped is refused rather than answered from its clipped centre — which is
how a sliver read as in view. The driver raises the refusal as
`ViewportUndecided` with the bridge's reason, keeps a missing answer a loud failure,
and refuses a value outside the grammar rather than reading it as `false`. The
conversations action's "proven in view" read folds the refusal into `False` (its
docstring says why that is sound for the selected-message journey).

The bridge's own decision is pinned by `--self-test-scroll-policy`
(`test_bridge_scroll_policy_windows.py`); the geometry end to end by
`test_windows_scroll_into_view_fraction.py`. Pure state here: the attribute read is
replaced with a canned answer and the driver is built without launching anything.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from actions.conversations import ConversationsActions  # noqa: E402
from drivers.base import ViewportUndecided  # noqa: E402
from drivers.http_bridge import HttpBridgeDriver  # noqa: E402
from drivers.windows import WindowsBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

WHY = "its bottom edge may be the viewport's clip (reading 286.3+52.2 in band 0+338.5)"


def _answering(monkeypatch, raw):
    monkeypatch.setattr(
        HttpBridgeDriver,
        "get_attr",
        lambda self, element_id, attribute, index=0, *, scope=None: raw,
    )
    monkeypatch.setattr(
        HttpBridgeDriver, "diagnose", lambda self, element_id, *, attrs=(), scope=None: "[diag]"
    )
    return object.__new__(WindowsBridgeDriver)


def test_true_and_false_answer_as_booleans(monkeypatch):
    assert _answering(monkeypatch, "true").in_viewport("post-card", index=3) is True
    assert _answering(monkeypatch, "false").in_viewport("post-card", index=3) is False


def test_an_undecided_answer_raises_with_the_bridges_reason(monkeypatch):
    driver = _answering(monkeypatch, f"undecided: {WHY}")
    with pytest.raises(ViewportUndecided) as raised:
        driver.in_viewport("post-card", index=3)
    message = str(raised.value)
    assert "'post-card'[3]" in message and WHY in message


def test_undecided_is_an_assertion_error_so_an_unprepared_caller_fails_loudly():
    assert issubclass(ViewportUndecided, AssertionError)


def test_no_answer_is_a_loud_failure_never_false(monkeypatch):
    driver = _answering(monkeypatch, None)
    with pytest.raises(AssertionError, match="answered no in-viewport") as raised:
        driver.in_viewport("post-card")
    assert not isinstance(raised.value, ViewportUndecided)


def test_a_value_outside_the_grammar_is_refused_not_read_as_false(monkeypatch):
    # The pre-2026-09 bridge answered an unknown attribute with HelpText; a value
    # like that must never pass as "not in view".
    driver = _answering(monkeypatch, "selected")
    with pytest.raises(AssertionError, match="grammar"):
        driver.in_viewport("post-card")


class _Thread:
    """A driver double listing two message bubbles, the target second."""

    def __init__(self, answer):
        self._answer = answer

    def get_texts(self, element_id, *, scope=None):
        assert element_id == "dm-message-text"
        return ["filler message 0", "the target message"]

    def in_viewport(self, element_id, *, index=0, scope=None):
        assert (element_id, index) == ("dm-message-text", 1)
        if isinstance(self._answer, Exception):
            raise self._answer
        return self._answer


def test_the_conversations_read_is_proven_in_view_only():
    assert ConversationsActions(_Thread(True)).message_in_view("target") is True
    assert ConversationsActions(_Thread(False)).message_in_view("target") is False
    undecided = ConversationsActions(_Thread(ViewportUndecided(WHY)))
    assert undecided.message_in_view("target") is False
    assert ConversationsActions(_Thread(True)).message_in_view("not listed") is False


def test_the_conversations_read_still_fails_on_a_missing_answer():
    with pytest.raises(AssertionError, match="no answer"):
        ConversationsActions(_Thread(AssertionError("no answer"))).message_in_view("target")
