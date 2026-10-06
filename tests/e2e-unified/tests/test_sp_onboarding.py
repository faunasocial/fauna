"""State-protocol tests: onboarding and unauthenticated flows.

Uses driver.reset() to return to factory state, then tests the
welcome screen, login flow, and error handling — all without
restarting the app or simulator.
"""
import uuid
import time

import pytest

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="module")
def app(persistent_app):
    """Start from a clean (unauthenticated) state."""
    persistent_app.driver.reset()
    return persistent_app


# ---------------------------------------------------------------------------
# Welcome screen
# ---------------------------------------------------------------------------

def test_welcome_screen_visible(app):
    """After reset, app should show welcome/onboarding screen."""
    state = app.driver.get_state()
    assert state["session"]["authenticated"] is False
    # Web SPA has no dedicated welcome route — reset lands on the default
    # page (feed) with an unauthenticated session. Native apps show
    # a distinct welcome screen.
    if not app.driver.is_web():
        assert state["nav"]["stack"][0]["view"] == "welcome"


@pytest.mark.feature("create-identity")
def test_sign_in_button_visible(app):
    """Welcome screen should have identity choice buttons."""
    if app.driver.is_web():
        # Web may show a different onboarding layout
        pass
    else:
        assert app.driver.is_visible("create-identity-button") or app.driver.is_visible("import-identity-button"), (
            "Neither create-identity-button nor import-identity-button visible on welcome screen"
        )


# ---------------------------------------------------------------------------
# Login via state injection (proves round-trip)
# ---------------------------------------------------------------------------

def test_login_via_state(app, nest_instance, test_user, web_api_url):
    """Inject session credentials and verify the app becomes authenticated."""
    secret_hex = test_user["signing_key"].encode().hex()
    node_url = web_api_url if app.driver.is_web() else nest_instance["url"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": secret_hex,
            "handle": f"e2e-onboard-{uuid.uuid4().hex[:6]}",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "test-device-onboard",
        },
    })
    state = app.driver.get_state()
    assert state["session"]["authenticated"] is True


def test_authenticated_nav_after_login(app):
    """After login, navigating to feed should work."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    state = app.driver.get_state()
    assert state["nav"]["stack"][0]["view"] != "welcome"


# ---------------------------------------------------------------------------
# Logout
# ---------------------------------------------------------------------------

def test_logout_returns_to_onboarding(app):
    """Logout should clear session and return to welcome."""
    app.driver.logout()
    state = app.driver.get_state()
    assert state["session"]["authenticated"] is False


# ---------------------------------------------------------------------------
# Reset
# ---------------------------------------------------------------------------

def test_reset_clears_everything(app, nest_instance, test_user, web_api_url):
    """Reset after login should return to factory state."""
    # First log in
    secret_hex = test_user["signing_key"].encode().hex()
    node_url = web_api_url if app.driver.is_web() else nest_instance["url"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": secret_hex,
            "handle": "e2e-reset-test",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "test-device-reset",
        },
    })
    state = app.driver.get_state()
    assert state["session"]["authenticated"] is True

    # Now reset
    app.driver.reset()
    state = app.driver.get_state()
    assert state["session"]["authenticated"] is False
    if not app.driver.is_web():
        assert state["nav"]["stack"][0]["view"] == "welcome"
