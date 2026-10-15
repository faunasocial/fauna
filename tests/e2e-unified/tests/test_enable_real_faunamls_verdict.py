"""`ConversationsActions.enable_real_faunamls`'s timeout names its own cause.

The real engine starts at AuthSuccess, so the poll can only time out on an app
that never signed in — and its message used to blame MLS regardless. A linux
launch once acknowledged the fixture's sign-in `set_state` without applying it,
and the red read "real FaunaMls backend did not activate within 60s". Convention 6: the failure says the app is not
signed in. The decision is pure logic over a driver's answers, so a fake driver
pins both branches in-process.
"""
import pytest

from actions.conversations import ConversationsActions

pytestmark = pytest.mark.tier_1


class _FakeDriver:
    """A linux driver whose probe acks, whose backend never goes live, and
    whose screen shows exactly ``visible``."""

    def __init__(self, visible: set[str]):
        self._visible = visible

    def is_macos(self):
        return False

    is_ios = is_android = is_macos

    def call_command(self, *_a, **_kw):
        return None

    def get_state(self, _path=None, **_kw):
        return False

    def is_visible(self, element_id):
        return element_id in self._visible

    def diagnose(self, element_id, **_kw):
        return f"[{element_id}: visible={element_id in self._visible}]"


def test_an_app_that_never_signed_in_is_named_not_signed_in():
    actions = ConversationsActions(_FakeDriver(visible=set()))
    with pytest.raises(AssertionError) as exc:
        actions.enable_real_faunamls(timeout_s=0.01)
    assert "the app is not signed in" in str(exc.value)
    assert "[feed-view: visible=False]" in str(exc.value)


def test_a_signed_in_app_keeps_the_mls_verdict():
    actions = ConversationsActions(_FakeDriver(visible={"feed-view"}))
    with pytest.raises(AssertionError) as exc:
        actions.enable_real_faunamls(timeout_s=0.01)
    assert "real FaunaMls backend did not activate" in str(exc.value)
    assert "not signed in" not in str(exc.value)
