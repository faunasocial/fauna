"""Smoke tests: verify each page loads without crashing.

Navigates to each page via state protocol and verifies the app
acknowledged the navigation (state round-trip).  For pages that have
a universally-implemented landmark element, also verifies the UI
rendered.  Dedicated per-feature tests (test_sp_authenticated, etc.)
cover element-level checks.
"""
import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


# Pages to test, in sidebar order.
# "groups" is intentionally absent: it is not a canonical page in ui.yaml
# (no `groups` in pages/tabs) — groups were merged into `conversations` via the
# ConversationsManager convergence.
# "devices" is intentionally absent too: the 2026-06-28 sync/folder UI
# unification split it into a Settings sub-page on every migrated app
# (`actions/backups.py::_folders_in_settings()` — web/linux/macos/ios/windows/tui;
# only android's own restructure is still pending), so a bare
# `{"view": "devices"}` nav no longer round-trips anywhere except android.
# Dedicated devices/folders tests (test_backups.py, test_device_cards.py, etc.)
# already cover the page via the app-aware `navigate_devices()` helper.
PAGES = [
    "feed",
    "conversations",
    "events",
    "contacts",
    "media",
    "settings",
    "backups",
    "bridges",
]

# Functional elements that are universally implemented on all apps
# and always visible when the page loads.  Only listed here if we are
# confident every app has the element — otherwise the state-protocol
# round-trip is sufficient for the smoke test.
# Use the ui.yaml-documented canonical settings landmark (`account-settings-link`,
# a shared `settings` page element + the iOS page landmark per ui.yaml notes),
# present on every app (linux: status.rs sets it). The previous
# `settings-handle-label` is a registry orphan — not in the settings page's
# `elements` and absent on linux (web/windows/apple show a handle label on
# settings; linux's parity gap is a separate linux-area follow-up).
_LANDMARKS = {
    "settings": "account-settings-link",
}


class TestPageLoads:
    """Navigate to each page via state protocol and verify it rendered."""

    @pytest.fixture(scope="class")
    def app(self, persistent_app, nest_instance, test_user, web_api_url):
        """Log in once for all page-load tests."""
        secret_hex = test_user["signing_key"].encode().hex()
        node_url = web_api_url if persistent_app.driver.is_web() else nest_instance["url"]
        persistent_app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "e2e-nav-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-nav",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        return persistent_app

    @pytest.mark.parametrize("page", PAGES)
    def test_page_loads(self, app, page):
        """Navigate via state protocol, verify the page loaded."""
        app.driver.set_state({"nav": {"stack": [{"view": page}]}})

        # State round-trip: the app acknowledged the navigation
        state = app.driver.get_state()
        assert state is not None, "get_state() returned None — bridge may have died"
        assert state["nav"]["stack"][0]["view"] == page

        # If a universal landmark exists, verify UI actually rendered
        landmark = _LANDMARKS.get(page)
        if landmark:
            app.driver.wait_for(landmark, timeout=10)
