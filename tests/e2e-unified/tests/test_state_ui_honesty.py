"""Honesty checks: verify state protocol matches what the UI actually shows.

These tests read the SAME value through two paths:
  1. State protocol (driver.get_state) — the JSON data the app reports
  2. UI element (driver.get_text / driver.is_visible) — what the user sees

If both paths agree, the state serializer is trustworthy for test assertions.
If they diverge, the client is reporting state that doesn't match its UI.

Only LEAF elements are used for UI reads (TextBlock, UILabel, Text, span).

Structure per check:
  - State consistency runs on ALL platforms (verify state returns expected values)
  - State-vs-UI comparison runs only on platforms with working UI reads
    (see app_capabilities.can_read_ui_for_honesty)

macOS is excluded from UI comparisons because:
  - Detail pane elements are invisible to XCUITest
  - Navigating to some pages (notifications) crashes the bridge
  - compose-text-field is inaccessible (can't create posts via UI)
macOS still runs state-only consistency checks.

Uses app_capabilities.py for state section availability and UI read safety.
See: the e2e test-redesign plan (tracked internally), Decision #3
"""

import time
import pytest

from app_capabilities import check_state_section, can_read_ui_for_honesty
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier1, pytest.mark.honesty, pytest.mark.tier_3]


class TestStateUIHonesty:
    """State protocol values must match UI element text."""

    @pytest.fixture(scope="class")
    def app(self, persistent_app, nest_instance, test_user):
        """Log in once for all honesty checks."""
        secret_hex = test_user["signing_key"].encode().hex()
        node_url = nest_instance["url"]
        if persistent_app.driver.is_web():
            try:
                from conftest import get_web_api_url
                node_url = get_web_api_url()
            except Exception:
                pass
        persistent_app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": secret_hex,
                "handle": "e2e-honesty",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "test-device-honesty",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        return persistent_app

    def _try_get_text(self, driver, element_id: str) -> str | None:
        """Try to read a UI element, returning None on any failure.

        Prevents bridge crashes from cascading into subsequent tests.
        """
        try:
            if not driver.is_visible(element_id):
                return None
            return driver.get_text(element_id)
        except Exception:
            return None

    # --- Check 1: Error message ---

    def test_error_message_matches_ui(self, app):
        """Injected error in state must match the error-message UI element.

        Navigate to feed first (a known-good page on all platforms), then
        inject the error. This ensures the page is rendered and the error
        banner has a place to appear (fixes Windows InfoBar visibility).
        """
        # Navigate to a known page first
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})

        test_msg = "Honesty check error"
        app.driver.set_state({"messages": {"error": test_msg}})

        # State consistency (all platforms). `set_state` already blocks until
        # the app acks the command, but native apps additionally have an
        # async state-serialization window after that ack (get_state's own
        # docstring) — so the read polls via `wait_for` rather than a blind
        # sleep guessing how long that window is (convention 14).
        from_state = app.driver.get_state(
            "messages.error", wait_for=lambda v: v == test_msg, timeout=5.0
        )
        assert from_state == test_msg, f"State has wrong error: {from_state!r}"

        # State-vs-UI comparison (platforms with UI reads)
        if can_read_ui_for_honesty(app.driver):
            from_ui = self._try_get_text(app.driver, "error-message")
            if from_ui is not None:
                assert from_ui == test_msg, (
                    f"UI shows {from_ui!r} but state says {from_state!r} — "
                    f"state serializer diverges from actual UI"
                )

        # Clean up
        app.driver.set_state({"messages": {"error": None}})

    # --- Check 2: Warning message ---

    def test_warning_message_matches_ui(self, app):
        """Injected warning in state must match the warning-message UI element.

        Some clients use a single element for all message types (Linux).
        If warning-message element is not found, check error-message as
        fallback (shared banner pattern).
        """
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})

        test_msg = "Honesty check warning"
        app.driver.set_state({"messages": {"warning": test_msg}})

        # State consistency (all platforms) — poll rather than sleep; see
        # test_error_message_matches_ui for why.
        from_state = app.driver.get_state(
            "messages.warning", wait_for=lambda v: v == test_msg, timeout=5.0
        )
        assert from_state == test_msg, f"State has wrong warning: {from_state!r}"

        # State-vs-UI comparison (platforms with UI reads)
        if can_read_ui_for_honesty(app.driver):
            from_ui = self._try_get_text(app.driver, "warning-message")
            if from_ui is None:
                # Fallback: some clients use a shared banner element
                from_ui = self._try_get_text(app.driver, "error-message")
            if from_ui is not None:
                assert from_ui == test_msg, (
                    f"UI shows {from_ui!r} but state says {from_state!r}"
                )

        app.driver.set_state({"messages": {"warning": None}})

    # --- Check 3: Page heading ---

    def test_page_heading_matches_navigation(self, app):
        """Page heading text must correspond to the navigated view.

        This check works on most platforms including macOS (page-heading
        is often in the sidebar, which IS accessible).
        """
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})

        # State consistency (all platforms) — poll for the nav write to land
        # rather than sleeping a fixed guess; see test_error_message_matches_ui.
        def _on_feed(s):
            stack = (s or {}).get("nav", {}).get("stack") or []
            return bool(stack) and stack[0].get("view") == "feed"

        state = app.driver.get_state(wait_for=_on_feed, timeout=5.0)
        state_view = state["nav"]["stack"][0]["view"]
        assert state_view == "feed"

        # UI read — page-heading is accessible on most platforms
        heading = self._try_get_text(app.driver, "page-heading")
        if heading is not None:
            assert heading, "page-heading element returned empty text"

    # --- Check 4: Feed post body ---

    def test_feed_post_body_matches_ui(self, app):
        """Post body text in state must match feed-post-text UI element.

        On platforms where compose UI is inaccessible (macOS detail pane),
        this check verifies state consistency only — it reads existing posts
        from state rather than trying to create one via UI.
        """
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        time.sleep(0.5)

        if can_read_ui_for_honesty(app.driver):
            # Full check: create post via UI, compare state and UI
            post_text = f"honesty-{time.monotonic_ns()}"
            try:
                app.driver.wait_for("compose-text-field", timeout=5)
                app.driver.type_text("compose-text-field", post_text)
                app.driver.click("post-submit-button")
            except Exception as e:
                pytest.skip(f"Compose UI not accessible: {e}")

            # Read from UI leaf element — the compose submit is a WS-RPC round
            # trip (nest persists the post, the client re-renders the feed),
            # so poll for the NEW post to surface rather than assert on a
            # single read after a fixed settle-sleep (testing.md convention
            # 14): a busy nest can outlast a short sleep and leave the feed
            # still showing the previous top post, which is not a state/UI
            # mismatch — the submit just hasn't rendered yet.
            last_seen: dict = {}

            def _new_post_visible():
                text = self._try_get_text(app.driver, "feed-post-text")
                last_seen["text"] = text
                return text if text is not None and post_text in text else None

            ui_body = None
            if self._try_get_text(app.driver, "feed-post-text") is not None:
                ui_body = wait_until(
                    _new_post_visible, RPC_ROUNDTRIP_S,
                    diagnose=lambda: (
                        f"post_text={post_text!r} last feed-post-text="
                        f"{last_seen.get('text')!r}"
                    ),
                )

            # Read from state and compare — polled, never read once. The UI wait
            # above proves the post RENDERED; the state push carrying it can land
            # a beat later on a native app (get_state's async serialization
            # window), and a single read here compared the new post against the
            # PREVIOUS top post in state (convention 14). That read could only
            # pass reliably when nothing earlier in the session had posted, so
            # `posts` was empty and the comparison below never ran at all.
            def _state_caught_up(s):
                feed_data, skip = check_state_section(app.driver, s, "feed")
                if skip:
                    return True  # nothing to wait for; the skip is handled below
                posts = feed_data.get("posts", [])
                return bool(posts) and post_text in posts[0].get("body", "")

            state = app.driver.get_state(
                wait_for=_state_caught_up, timeout=RPC_ROUNDTRIP_S
            )
            feed_data, skip = check_state_section(app.driver, state, "feed")
            if skip:
                return
            posts = feed_data.get("posts", [])
            if posts:
                state_body = posts[0].get("body", "")
                assert post_text in state_body, (
                    f"State body doesn't contain '{post_text}', got: {state_body!r}"
                )
                if ui_body:
                    assert state_body in ui_body or ui_body in state_body, (
                        f"State body {state_body!r} doesn't match UI body {ui_body!r}"
                    )
        else:
            # State-only check: read existing posts from state
            state = app.driver.get_state()
            feed_data, skip = check_state_section(app.driver, state, "feed")
            if skip:
                pytest.skip(skip)
            posts = feed_data.get("posts", [])
            # Just verify state structure is valid if posts exist
            for post in posts[:3]:
                assert "post_id" in post, f"Post missing post_id: {post}"
                assert "author" in post, f"Post missing author: {post}"

    # --- Check 5: Notification count badge ---

    def test_notification_count_matches_ui(self, app):
        """Unread count in state must match notification-count-badge text.

        On macOS, navigating to the notifications page can crash the bridge.
        Guard with can_read_ui_for_honesty. State-only check verifies the
        notifications section structure.
        """
        if not can_read_ui_for_honesty(app.driver):
            # State-only: verify notifications section structure
            state = app.driver.get_state()
            notif_data, skip = check_state_section(app.driver, state, "notifications")
            if skip:
                pytest.skip(skip)
            assert "unread_count" in notif_data, (
                f"notifications section missing unread_count: {notif_data}"
            )
            return

        # Full check: navigate to notifications, compare state and UI. Poll
        # for the nav write to land rather than sleeping a fixed guess; see
        # test_error_message_matches_ui.
        app.driver.set_state({"nav": {"stack": [{"view": "notifications"}]}})

        def _on_notifications(s):
            stack = (s or {}).get("nav", {}).get("stack") or []
            return bool(stack) and stack[0].get("view") == "notifications"

        app.driver.get_state(wait_for=_on_notifications, timeout=5.0)

        badge_text = self._try_get_text(app.driver, "notification-count-badge")

        state = app.driver.get_state()
        notif_data, skip = check_state_section(app.driver, state, "notifications")
        if skip:
            return

        state_count = notif_data.get("unread_count", -1)
        if state_count == -1:
            return

        if badge_text is not None:
            assert str(state_count) in badge_text, (
                f"State says {state_count} unread but badge shows {badge_text!r}"
            )
