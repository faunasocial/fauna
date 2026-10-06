import pytest

# Standalone Moderation queue render-confirm. Marked with the apps that render
# a dedicated `moderation-queue` and whose test-agent nav reaches it. iOS joined
# 2026-06-30: the shared FaunaKit `ModerationQueueView` always rendered on iOS,
# but the iOS test-agent nav router (`Fauna-iOS/App/FaunaApp.swift` `applyNavPatch`
# → the `moreTabs` set) used to omit `"moderation"`, so `navigate_to("moderation")`
# fell to the `default:` "Unknown nav view" no-op and never reached the queue.
# A fix added `"moderation"` to `moreTabs` (the exact peer of
# bridges/notifications: macOS-sidebar → iOS-More), so the state-protocol nav now
# routes and the queue renders — `pytest.mark.ios` re-added + e2e-confirmed. web
# embeds moderation in Settings (moderation.md § Architectural rules: "acceptable
# as a layout choice iff the same IDs render") and now also renders the
# `moderation-queue` container around its rows, so every app asserts the same way.
pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.web,
    pytest.mark.tui,
    pytest.mark.android,
]


@pytest.mark.feature("moderation-queue")
def test_navigate_to_moderation(logged_in_app):
    """Navigate to the standalone Moderation queue and confirm it renders.

    The queue lists the caller's own flagged/actioned content (moderation.md
    § Goal). A fresh nest configures no obligation rules, so the queue is empty —
    that's the empty state, NOT an error (moderation.md § Errors & edge cases).
    Every app (standalone linux/windows/macos/ios/android, embedded-in-Settings
    web) renders a dedicated `moderation-queue` container even when empty (the
    scope anchor for e2e) — same IDs either way (moderation.md § Architectural
    rules). Asserting the container is visible — not merely "no error" — is what
    proves the view actually loaded rather than silently rendering nothing.
    """
    logged_in_app.moderation.navigate()
    assert not logged_in_app.has_error()
    assert logged_in_app.driver.is_visible("moderation-queue")


@pytest.mark.feature("moderation-queue")
def test_moderation_correction_count(logged_in_app):
    """A fresh nest carries no obligation rules, so the queue is empty and
    exposes zero `train-correction-button`s on every app (moderation.md
    § State & data shape; the nest-side obligation/classifier stack is retired —
    content-scoring.md § Implementation status — so `obligation_action_records`
    is never populated; see the in-tree note in
    tests/api/test_content_moderation.py). The count is reachable and zero."""
    logged_in_app.moderation.navigate()
    assert logged_in_app.moderation.correction_count() == 0
