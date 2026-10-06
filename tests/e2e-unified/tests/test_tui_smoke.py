"""Smoke for the cli client: real fauna-tui binary in a pty, real nest.

The first tier_3 coverage of the 7th client: the
driver launches `fauna-tui` under FAUNA_E2E_AGENT_PORT (via the `app`
fixture, which also spins the shared `nest_instance`) and drives the
in-process agent — the unauthenticated onboarding surface, the session-patch
login (the same `set_state` seam `logged_in_app` uses), the authenticated
sidebar shell, and the state protocol.

Run: pytest tests/test_cli_smoke.py --client tui
(cli is not in the default parametrization until M2 completes.)
"""

import uuid

import pytest

pytestmark = [pytest.mark.tier0, pytest.mark.tier_3, pytest.mark.tui]

_TABS = (
    "feed-tab",
    "conversations-tab",
    "contacts-tab",
    "profile-tab",
    "events-tab",
    "media-tab",
    "backups-tab",
    "nostr-tab",
    "bridges-tab",
    "notifications-tab",
    "settings-tab",
)


def test_unauthenticated_surface(app):
    """Fresh (reset) app: onboarding identity choice, nav hidden."""
    d = app.driver
    assert d.get_state("session")["authenticated"] is False
    # The identity-choice page is the unauthenticated surface.
    assert d.is_visible("create-identity-button")
    assert d.is_visible("import-identity-button")
    # Navigation is hidden while unauthenticated (ui.yaml hidden_when).
    assert d.is_absent("feed-tab")
    # Global chrome: connection-status reads the honest signed-out state.
    assert d.get_text("connection-status") == "Disconnected"
    # Clean page: error-message must NOT read as visible.
    assert not d.is_visible("error-message")


def test_login_shows_shell_and_navigation_works(app, nest_instance, test_user):
    """Session-patch login → sidebar shell; click + set_state navigation."""
    d = app.driver
    secret_hex = test_user["signing_key"].encode().hex()
    app.auth.login(
        node_url=nest_instance["url"],
        username=test_user["actor_id_hex"],
        password="",
        secret_hex=secret_hex,
        handle=f"e2e-{uuid.uuid4().hex[:8]}",
    )
    assert d.get_state("session")["authenticated"] is True

    # All 11 canonical navigation.tabs rows are present exactly once.
    for tab in _TABS:
        assert d.count(tab) == 1, f"{tab} missing from the sidebar"

    # The feed page renders its canonical container.
    assert d.is_visible("feed-view")

    # A real click on a sidebar row lands on the page (registry action path).
    d.click("events-tab")
    assert d.get_state("nav")["stack"][-1]["view"] == "events"

    # set_state nav patch drives navigation the standard way.
    d.set_state({"nav": {"stack": [{"view": "settings"}]}})
    assert d.get_state("nav")["stack"][-1]["view"] == "settings"

    # The live WS-RPC connection against the real nest reaches Connected —
    # poll briefly; the supervisor connects asynchronously after login.
    d.wait_for_state(
        lambda s: s and s.get("session", {}).get("authenticated"), timeout=5
    )
    import time

    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if d.get_text("connection-status") == "Connected":
            break
        time.sleep(0.3)
    assert d.get_text("connection-status") == "Connected", (
        "NestClient should reach Connected against the live nest_instance"
    )

    # reset() signs out: back to the onboarding surface, unauthenticated.
    d.reset()
    assert d.get_state("session")["authenticated"] is False
    assert d.is_visible("create-identity-button")
    assert d.is_absent("feed-tab")
