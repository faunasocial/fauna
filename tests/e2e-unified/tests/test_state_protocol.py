"""Integration tests for the unified test state protocol.

These tests verify the full round-trip: Python driver → bridge relay →
app test agent → state serialization → bridge cache → Python driver.

Requires a running app with the test agent enabled (FAUNA_E2E_BRIDGE set).
"""
import pytest

pytestmark = pytest.mark.tier_3


class TestStateProtocol:
    """Verify the state protocol works end-to-end."""

    @pytest.fixture(scope="class")
    def app(self, persistent_app):
        return persistent_app

    def test_initial_state_is_readable(self, app):
        """Agent should push initial state within a few seconds of launch."""
        state = app.driver.get_state()
        assert state is not None
        assert "session" in state
        assert "nav" in state

    def test_set_session_state(self, app, nest_instance, test_user, web_api_url):
        """Setting session state should authenticate the app."""
        secret_hex = test_user["signing_key"].encode().hex()
        node_url = web_api_url if app.driver.is_web() else nest_instance["url"]
        app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "protocol-test",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-protocol",
            },
        })
        state = app.driver.get_state()
        assert state["session"]["authenticated"] is True
        assert state["session"]["handle"] == "protocol-test"

    def test_navigate_via_state(self, app):
        """Setting nav state should change the current view."""
        app.driver.set_state({"nav": {"stack": [{"view": "conversations"}]}})
        state = app.driver.get_state()
        assert state["nav"]["stack"][0]["view"] != "welcome"

    def test_reset_returns_to_onboarding(self, app):
        """Reset should clear session and return to welcome screen."""
        app.driver.reset()
        state = app.driver.get_state()
        assert state["session"]["authenticated"] is False

    def test_round_trip_after_reset(self, app, nest_instance, test_user, web_api_url):
        """After reset, setting state should work again."""
        secret_hex = test_user["signing_key"].encode().hex()
        node_url = web_api_url if app.driver.is_web() else nest_instance["url"]
        app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "round-trip",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-rt",
            },
        })
        state = app.driver.get_state()
        assert state["session"]["authenticated"] is True
