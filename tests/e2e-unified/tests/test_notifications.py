import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("notifications")
def test_navigate_to_notifications(logged_in_app):
    """Navigate to notifications page and verify it loads without error.

    The page hydrates via ``fauna.notifications.list`` on mount; a client still
    calling the deleted ``GET /api/v1/notifications/{actor}`` HTTP route (now a
    404) surfaces the rejection on the page's ``error-message`` banner. The
    element-visibility checks below pass regardless of that error, so assert the
    list load succeeded too.
    """
    logged_in_app.notifications.navigate()
    assert logged_in_app.driver.is_visible("notification-mark-read"), (
        "notifications page should show the mark-read control: "
        f"{logged_in_app.driver.diagnose('notification-mark-read')}"
    )
    assert logged_in_app.driver.is_visible("notification-count-badge"), (
        "notifications page should show the count badge: "
        f"{logged_in_app.driver.diagnose('notification-count-badge')}"
    )
    assert not logged_in_app.has_error(), (
        f"notifications page surfaced an error after load: "
        f"{logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("notifications")
def test_mark_notifications_read(logged_in_app):
    """Mark all notifications as read."""
    logged_in_app.notifications.navigate()
    logged_in_app.notifications.mark_all_read()
    badge = logged_in_app.notifications.unread_badge_text()
    assert badge == "" or badge == "0 unread", (
        "after mark-all-read the unread badge should be empty or '0 unread'; "
        f"got {badge!r}: {logged_in_app.driver.diagnose('notification-count-badge')}"
    )
