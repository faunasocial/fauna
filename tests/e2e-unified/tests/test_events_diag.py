"""Diagnostic for events page element visibility."""
import time
import pytest

pytestmark = [pytest.mark.tier0, pytest.mark.tier_3]


def test_events_elements(logged_in_app):
    """Check what's visible on the events page."""
    driver = logged_in_app.driver

    # Navigate
    driver.navigate_to("events")
    time.sleep(2)

    elements = [
        "page-heading",
        "calendar-view-agenda", "calendar-view-month",
        "calendar-view-week", "calendar-view-day",
        "events-view-toggle",
        "events-prev-month", "events-next-month",
        "new-event-btn",
        "calendar-week-grid", "calendar-day-timeline",
        "events-month-grid",
        "error-message",
    ]
    for eid in elements:
        visible = driver.is_visible(eid)
        print(f"  {eid}: {'VISIBLE' if visible else 'not found'}")

    # Try reading heading text
    try:
        heading = driver.get_text("page-heading")
        print(f"  page-heading text: {heading!r}")
    except Exception as e:
        print(f"  page-heading text ERROR: {e}")

    # Check state
    state = driver.get_state()
    nav = state.get("nav", {}).get("stack", [{}])[0].get("view") if state else None
    print(f"  nav view: {nav}")
    error = state.get("messages", {}).get("error") if state else None
    print(f"  messages.error: {error!r}")

    assert driver.is_visible("calendar-view-agenda"), "calendar-view-agenda not visible"
