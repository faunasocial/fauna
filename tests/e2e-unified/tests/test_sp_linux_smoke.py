"""Smoke test: verify the Linux test agent pushes state and accepts commands."""
import time

import pytest

pytestmark = pytest.mark.tier_2


@pytest.fixture(scope="module")
def app(persistent_app):
    return persistent_app


def test_agent_pushes_initial_state(app):
    """Agent should push state within seconds of launch."""
    state = app.driver.get_state()
    assert state is not None, "Agent never pushed state"
    assert "session" in state
    assert "nav" in state


def test_agent_acknowledges_patch(app):
    """Agent should acknowledge a patch command."""
    app.driver.set_state({"session": {"authenticated": True}})
    state = app.driver.get_state()
    assert state["session"]["authenticated"] is True


def test_authenticate_for_nav(app, nest_instance, test_user):
    """Authenticate so the main window stack exists for navigation tests."""
    secret_hex = test_user["signing_key"].encode().hex()
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest_instance["url"],
            "secret_hex": secret_hex,
            "handle": "linux-smoke",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "test-device-smoke",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    time.sleep(2)
    state = app.driver.get_state()
    assert state["session"]["authenticated"] is True
    assert state["nav"]["stack"][0]["view"] != "welcome"


def test_nav_state_reflects_stack(app):
    """Navigate via set_state and verify the view changed."""
    app.driver.set_state({"nav": {"stack": [{"view": "contacts"}]}})
    time.sleep(0.5)
    state = app.driver.get_state()
    assert state["nav"]["stack"][0]["view"] == "contacts"


@pytest.mark.parametrize("page", [
    "conversations", "groups", "events", "contacts",
    "feed", "media", "backups",
])
def test_navigate_via_state(app, page):
    """set_state with nav should switch the GTK stack page."""
    app.driver.set_state({"nav": {"stack": [{"view": page}]}})
    time.sleep(0.5)
    state = app.driver.get_state()
    assert state["nav"]["stack"][0]["view"] == page
