"""Diagnostic for settings page elements."""
import time
import pytest

pytestmark = [pytest.mark.tier0, pytest.mark.tier_3]


def test_settings_elements(logged_in_app):
    """Check what's visible on the settings page."""
    driver = logged_in_app.driver
    # Inbox-mode/spam live on the Privacy sub-page of the Settings sidebar-swap
    # shell (linux); single-scroll clients ignore the sub-id and show everything.
    driver.set_state({
        "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "privacy"}]},
    })
    time.sleep(2)

    elements = [
        "page-heading",
        "inbox-mode-open", "inbox-mode-allow_knock",
        "inbox-mode-contacts_only", "inbox-mode-closed",
        "spam-preferences",
        "settings-autostart-toggle",
        "error-message",
    ]
    for eid in elements:
        visible = driver.is_visible(eid)
        print(f"  {eid}: {'VISIBLE' if visible else 'not found'}")

    try:
        heading = driver.get_text("page-heading")
        print(f"  page-heading text: {heading!r}")
    except Exception as e:
        print(f"  page-heading text ERROR: {e}")

    state = driver.get_state()
    nav = state.get("nav", {}).get("stack", [{}])[0].get("view") if state else None
    print(f"  nav view: {nav}")

    assert driver.is_visible("inbox-mode-open"), "inbox-mode-open not visible"
