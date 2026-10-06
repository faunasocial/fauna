"""tier_1: `drivers/android.py`'s `type_text` routing, against recorders.

Authority: `docs/goal/ui/conversations.md` § Reactions & message delete →
*Rendering / picker glue* — android's fuller picker is the emoji2
`EmojiPickerView`, and its e2e seam is the one every app shares: click
`dm-reaction-more-button`, then `type_text` the emoji at it. UiAutomator cannot
type into a hosted native chooser, so the driver hands exactly the
`AGENT_TYPED_TARGETS` ids to the app's agent as a `type_text` patch
(`TestAgent.kt::pickMoreReaction`, Robolectric-pinned by
`TestAgentReactionPickTest`) and every other id to the bridge's
`/element/type`, unchanged. No android e2e run venue exists yet, so this is
the one place the driver half of that seam executes.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1


@pytest.fixture
def recorded():
    driver = AndroidBridgeDriver()
    calls: list[tuple] = []
    driver.set_state = lambda state, *a, **k: calls.append(("set_state", state))
    driver._post_with_scroll = lambda path, body: calls.append(("post", path, body))
    return driver, calls


def test_the_more_reaction_button_types_through_the_agent(recorded):
    driver, calls = recorded
    driver.type_text("dm-reaction-more-button", "🦊")
    assert calls == [
        ("set_state", {"type_text": {"target": "dm-reaction-more-button", "text": "🦊"}}),
    ]


def test_every_other_id_still_types_through_the_bridge(recorded):
    driver, calls = recorded
    driver.type_text("dm-text-field", "hello")
    assert calls == [("post", "/element/type", {"id": "dm-text-field", "text": "hello"})]
