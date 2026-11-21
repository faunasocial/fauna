"""wait_until reports a raising ``diagnose`` beside the timeout, and the shared
error reader agrees with ``has_error`` (convention 6: a diagnostic never
replaces the failure it describes)."""
import pytest

from actions import ActionLayer
from helpers import waiting

pytestmark = pytest.mark.tier_1


def test_raising_diagnose_still_fails_with_the_timeout():
    def boom():
        raise LookupError("element not found: error-message")

    with pytest.raises(AssertionError) as info:
        waiting.wait_until(lambda: False, 0.05, interval=0.01, diagnose=boom)
    message = str(info.value)
    assert "timed out after" in message
    assert "LookupError" in message and "element not found" in message


class _Driver:
    def __init__(self, state=None, visible=False, text_raises=False):
        self._state, self._visible, self._raises = state or {}, visible, text_raises

    def get_state(self, path=None):
        node = self._state
        for part in (path or "").split("."):
            if not part:
                continue
            if not isinstance(node, dict) or part not in node:
                return None
            node = node[part]
        return node

    def is_visible(self, _id):
        return self._visible

    def get_text(self, _id):
        if self._raises:
            raise LookupError("404")
        return "element text"


def _app(driver):
    app = ActionLayer.__new__(ActionLayer)
    app.driver = driver
    return app


def test_has_error_and_error_text_agree_when_only_state_publishes_the_error():
    # The apple DNS page case: messages.error set, no resolvable element.
    app = _app(_Driver(state={"messages": {"error": "refused"}}, text_raises=True))
    assert app.has_error()
    assert app.error_for_failure() == "refused"


def test_error_for_failure_is_none_without_an_error_and_never_raises():
    assert _app(_Driver(state={"messages": {"error": None}})).error_for_failure() == "none"
