"""tier_1: the android action arms the drafts-survive witnesses drive, against recorders.

Authority: `docs/goal/behavior/conversation-drafts.md` § Persistence — on
android the system back (`BackHandler` → `deactivateNewConversation()` then
`popBackStack()`, `NewThreadComposeScreen.kt`) steps out of the new-message
composer and keeps its draft; `docs/goal/ui/events.md` § Persistence — the
New Event opener resumes the draft. The arms under test:
`ConversationsActions.step_out_of_new_conversation` sends the system back
through the bridge's `POST /device/back` (`ElementOps.pressBack`), and
`ConversationsActions.compose_body_text` / `EventsActions.compose_summary_text`
read the field's own text. No android e2e run venue exists yet, so this is the
one place the driver half of these arms executes.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from actions.conversations import ConversationsActions  # noqa: E402
from actions.events import EventsActions  # noqa: E402
from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1


@pytest.fixture
def recorded():
    driver = AndroidBridgeDriver()
    calls: list[tuple] = []
    driver._post = lambda path, body=None, *a, **k: calls.append(("post", path, body))
    driver.get_text = lambda element_id, *a, **k: (
        calls.append(("get_text", element_id)) or f"text of {element_id}"
    )
    return driver, calls


def test_stepping_out_of_a_new_message_sends_the_system_back(recorded):
    driver, calls = recorded
    ConversationsActions(driver).step_out_of_new_conversation()
    assert calls == [("post", "/device/back", {})]


def test_the_message_body_is_read_from_the_compose_field(recorded):
    driver, calls = recorded
    assert ConversationsActions(driver).compose_body_text() == "text of dm-text-field"
    assert calls == [("get_text", "dm-text-field")]


def test_the_event_summary_is_read_from_the_form_field(recorded):
    driver, calls = recorded
    assert EventsActions(driver).compose_summary_text() == "text of event-summary"
    assert calls == [("get_text", "event-summary")]
