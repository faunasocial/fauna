"""Diagnostic: verify TestAgent stays alive across many state protocol commands.

If the agent stops polling after N commands, all subsequent set_state()
calls will time out.  This test catches that failure mode.
"""
import time
import pytest


pytestmark = [pytest.mark.tier0, pytest.mark.tier_3]


def test_agent_survives_rapid_navigation(persistent_app, nest_instance, test_user):
    """Send 10 rapid set_state commands and verify all are acknowledged."""
    app = persistent_app
    secret_hex = test_user["signing_key"].encode().hex()

    # Login once — use longer timeout because authenticate() is now async
    # (the app calls api.authenticate(secret:) before setting self.client)
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest_instance["url"],
            "secret_hex": secret_hex,
            "handle": "e2e-agent-health",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "test-device-health",
        },
        "nav": {"stack": [{"view": "feed"}]},
    }, timeout=30)

    # NB: no "groups" — the standalone Groups page was merged into
    # conversations (ConversationsManager convergence), so it is not a valid
    # nav target on any client; navigating to it would never land and the
    # `nav == page` assertion below would fail. Use only canonical pages.
    pages = ["conversations", "contacts", "events", "contacts", "media",
             "feed", "conversations", "media", "events", "feed"]

    for i, page in enumerate(pages):
        try:
            app.driver.set_state({"nav": {"stack": [{"view": page}]}})
        except TimeoutError:
            # Check if bridge is alive
            try:
                state = app.driver.get_state()
                last_cmd = state.get("last_command_id", "?")
                nav = state.get("nav", {}).get("stack", [{}])[0].get("view", "?")
                pytest.fail(
                    f"set_state timed out on command #{i+1} (page={page}).\n"
                    f"Bridge is alive. Last ack'd command: {last_cmd}, current nav: {nav}\n"
                    f"TestAgent likely stopped polling — check app logs for errors."
                )
            except Exception as e:
                pytest.fail(
                    f"set_state timed out on command #{i+1} (page={page}).\n"
                    f"Bridge is also dead: {e}\n"
                    f"App likely crashed."
                )

        state = app.driver.get_state()
        assert state["nav"]["stack"][0]["view"] == page, \
            f"Command #{i+1}: expected nav={page}, got {state['nav']}"
