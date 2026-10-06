"""Tests for the unified message display system (error / warning / info).

These tests verify the state protocol `messages` field and the ActionLayer
helpers that read it. Most tests will initially FAIL because no client
serializes `messages` into the state protocol yet -- that is expected.
As each app (Windows, macOS, Web, etc.) adds the `messages` state field,
these tests will start passing for that client.

Tests that don't depend on `messages` in state (e.g. the UI-element fallback
path) should pass today on clients that already have `error-message` elements.
"""
import time

import pytest

from helpers.app_surface import skip_unbuilt

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


class TestStateMessages:
    """Tests that require the `messages` key in the state protocol.

    These will fail until clients implement state serialization for messages.
    """

    @pytest.fixture(scope="class")
    def app(self, persistent_app, nest_instance, test_user, web_api_url):
        """Log in once for all tests in this class."""
        secret_hex = test_user["signing_key"].encode().hex()
        node_url = web_api_url if persistent_app.driver.is_web() else nest_instance["url"]
        persistent_app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "e2e-msg-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-messages",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        return persistent_app

    def test_state_has_messages_key(self, app):
        """After login, the state should contain a `messages` key.

        Expected state shape:
            {
                "messages": {
                    "error": null,
                    "warning": null,
                    "info": null
                }
            }

        This test will FAIL until a client implements the messages field
        in its state protocol serialization.
        """
        state = app.driver.get_state()
        assert state is not None, "State protocol returned None -- agent may not be running"
        if "messages" not in state:
            skip_unbuilt(
                app.driver,
                surface="the state protocol's `messages` field",
                detail="the unified message display system is not yet serialized into state on any client",
                tracked="test_messages.py module docstring",
            )
        messages = state["messages"]
        assert isinstance(messages, dict), f"messages should be a dict, got {type(messages)}"
        for level in ("error", "warning", "info"):
            assert level in messages, (
                f"messages is missing the '{level}' key. "
                f"Available keys: {list(messages.keys())}"
            )

    def test_messages_null_on_healthy_page(self, app):
        """On a healthy page with no errors, all message fields should be null.

        This verifies the happy path -- the app reports no messages when
        everything is working. Depends on test_state_has_messages_key passing.
        """
        state = app.driver.get_state()
        assert state is not None
        if "messages" not in state:
            pytest.skip("Client does not serialize messages into state yet")
        messages = state["messages"]
        assert messages.get("error") is None, (
            f"Expected no error on healthy page, got: {messages['error']}"
        )
        assert messages.get("warning") is None, (
            f"Expected no warning on healthy page, got: {messages['warning']}"
        )
        assert messages.get("info") is None, (
            f"Expected no info on healthy page, got: {messages['info']}"
        )


class TestActionLayerMessages:
    """Tests for the ActionLayer message helpers.

    These exercise error_text(), warning_text(), info_text() and their
    has_* counterparts. They should work today via the UI-element fallback
    even if the state protocol doesn't have messages yet.
    """

    def test_no_spurious_errors_on_healthy_page(self, logged_in_app):
        """After login on feed page, there should be no error message.

        This works via either state protocol or UI element fallback.
        """
        app = logged_in_app
        # Give the page a moment to settle after login
        time.sleep(2)
        error = app.error_text()
        assert error == "", f"Unexpected error on healthy page: {error!r}"
        assert not app.has_error(), "has_error() should be False on healthy page"

    def test_error_text_returns_string(self, logged_in_app):
        """error_text() must always return a string, never None or raise."""
        result = logged_in_app.error_text()
        assert isinstance(result, str)

    def test_warning_text_returns_string(self, logged_in_app):
        """warning_text() must always return a string, never None or raise."""
        result = logged_in_app.warning_text()
        assert isinstance(result, str)

    def test_info_text_returns_string(self, logged_in_app):
        """info_text() must always return a string, never None or raise."""
        result = logged_in_app.info_text()
        assert isinstance(result, str)

    def test_no_spurious_warnings_on_healthy_page(self, logged_in_app):
        """After login on feed page, there should be no warning message."""
        app = logged_in_app
        assert not app.has_warning(), "has_warning() should be False on healthy page"
        assert app.warning_text() == "", (
            f"Unexpected warning: {app.warning_text()!r}"
        )

    def test_no_spurious_info_on_healthy_page(self, logged_in_app):
        """After login on feed page, there should be no info message."""
        app = logged_in_app
        assert not app.has_info(), "has_info() should be False on healthy page"
        assert app.info_text() == "", (
            f"Unexpected info: {app.info_text()!r}"
        )


class TestErrorClearsOnNavigation:
    """Placeholder tests for error-clears-on-navigation behavior.

    These tests verify that navigating away from a page with an error
    clears the error message. The structure is ready but the actual
    error injection depends on the messages state field being writable,
    which requires client implementation.
    """

    @pytest.fixture(scope="class")
    def app(self, persistent_app, nest_instance, test_user, web_api_url):
        """Log in once for all tests in this class."""
        secret_hex = test_user["signing_key"].encode().hex()
        node_url = web_api_url if persistent_app.driver.is_web() else nest_instance["url"]
        persistent_app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "e2e-nav-clear",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-nav-clear",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        return persistent_app

    def test_error_clears_on_navigation(self, app):
        """Inject an error via state, navigate away, verify it clears.

        This test will FAIL until a client implements the messages state
        field AND supports patching messages via set_state.
        """
        state = app.driver.get_state()
        if state is None or "messages" not in state:
            pytest.skip("Client does not serialize messages into state yet")

        # Start from a page that is NOT the navigation target so the later
        # nav to "contacts" is a genuine cross-page navigation. The fixture
        # is class-scoped and a sibling test in this class can leave us on
        # "contacts"; on web a same-route nav is a no-op that never remounts
        # the page (so the per-page MessageBanner never resets), which would
        # make this a false test of "clears on navigation".
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        time.sleep(1)

        # Inject an error message
        app.driver.set_state({
            "messages": {"error": "Test error for navigation clearing"},
        })
        assert app.has_error(), "Error should be visible after injection"

        # Navigate to a different page (use "contacts", not "settings" —
        # macOS settings view crashes in AppKit layout)
        app.driver.set_state({"nav": {"stack": [{"view": "contacts"}]}})
        time.sleep(1)

        # Error should be cleared on the new page
        assert not app.has_error(), (
            f"Error should clear on navigation, but got: {app.error_text()!r}"
        )

    def test_warning_clears_on_navigation(self, app):
        """Inject a warning via state, navigate away, verify it clears."""
        state = app.driver.get_state()
        if state is None or "messages" not in state:
            pytest.skip("Client does not serialize messages into state yet")

        # Start from a non-target page so the nav to "contacts" below is a
        # real cross-page navigation (see test_error_clears_on_navigation).
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        time.sleep(1)

        app.driver.set_state({
            "messages": {"warning": "Test warning for navigation clearing"},
        })
        assert app.has_warning(), "Warning should be visible after injection"

        app.driver.set_state({"nav": {"stack": [{"view": "contacts"}]}})
        time.sleep(1)

        assert not app.has_warning(), (
            f"Warning should clear on navigation, but got: {app.warning_text()!r}"
        )


@pytest.mark.windows
def test_injected_error_survives_a_page_refresh(logged_in_app):
    """An injected error lives until a NAV, a clear, or `reset` — never until
    the current page's next re-render.

    `set_state({"messages": {"error": …}})` is a command, and convention 11's
    corollary rules that a HALF-APPLIED command is a dropped one
    (`e2e-conventions.md` § convention 11). The class above pins the lifetime's
    far end (a nav clears it); this pins the near end, which is the one that
    broke: windows wrote the injection into `App.CurrentErrorMessage`, the page
    mirror every page rewrites as it re-renders, and `FeedPage.Refresh()` runs on
    EVERY feed observer tick and nulls the mirror whenever the snapshot carries
    no error. So the injection read back as `None` whenever a tick landed between
    the patch and the read — `test_state_ui_honesty.py::test_error_message_matches_ui`
    failed 3/3 in a batch and passed alone, which is how it was once misread as a
    sleep race and "fixed" by polling.

    The honesty test can only ever hit that window by luck. This one forces it:
    a post submitted through the composer rendering in the list is a causal
    barrier (convention 14) — the feed list is rebuilt only by the page's
    re-render, so once the post is visible, that re-render has run AFTER the
    injection, and whatever slot holds the injection has faced it.

    **Why `windows`-marked rather than cross-app** — the same reason as
    `test_agent_refuses_unknown_command.py::test_the_refusal_slot_survives_a_nav_and_clears_at_reset`:
    windows is the app it was written for and run against. tui and linux hold
    the injection in a dedicated slot already (tui `App::injected_error`, linux
    the page's own label); a session running this against another app should add
    that app's mark.
    """
    app = logged_in_app
    app.driver.navigate_to("feed")

    injected = f"Injected error {time.monotonic_ns()}"
    app.driver.set_state({"messages": {"error": injected}})

    landed = app.driver.get_state(
        "messages.error", wait_for=lambda v: v == injected, timeout=5.0
    )
    assert landed == injected, (
        f"the injection never read back ({landed!r}) — either the patch was "
        "dropped, or the page's re-render already wiped it before the first read "
        "(the defect the barrier below exists to force deterministically)"
    )

    app.feed.create_post(f"refresh barrier {time.monotonic_ns()}")

    assert app.error_text() == injected, (
        f"the injected error did not survive the feed page's re-render: read "
        f"{app.error_text()!r}. The injection is in a slot the page rewrites on "
        "every observer tick (windows' `App.CurrentErrorMessage` page mirror) "
        "rather than a dedicated injected-message slot cleared only by nav, "
        "clear, or reset (tui's `App::injected_error`)."
    )

    app.driver.set_state({"messages": {"error": None}})
    assert app.error_text() == "", (
        f"an explicit `messages.error: null` must clear the injection, got "
        f"{app.error_text()!r}"
    )
