"""Diagnostic: what elements can XCUITest find on macOS?

Tests element visibility for known accessibility identifiers across
different pages to understand XCUITest's accessibility tree coverage.
"""
import time
import pytest


pytestmark = [pytest.mark.tier0, pytest.mark.tier_3]

# Elements we expect to find on each page
ONBOARDING_ELEMENTS = [
    "create-identity-button", "import-identity-button",
]

FEED_ELEMENTS = [
    "feed-view", "compose-text-field", "post-submit-button",
    "feed-post-text", "feed-tab",
]

CONTACTS_ELEMENTS = [
    "contact-actor-id-field", "contact-actor-id-lookup",
    "contact-name", "contacts-tab",
]

EVENTS_ELEMENTS = [
    "calendar-view-agenda", "calendar-view-month",
    "calendar-view-week", "calendar-view-day",
    "events-tab",
]

SIDEBAR_ELEMENTS = [
    "conversations-tab", "groups-tab", "contacts-tab",
    "events-tab", "feed-tab", "media-tab",
    "settings-tab",
]


def _check_elements(driver, element_ids: list[str]) -> dict[str, dict]:
    """Return count and is_visible for each element ID."""
    results = {}
    for eid in element_ids:
        count = driver.count(eid)
        visible = driver.is_visible(eid)
        results[eid] = {"count": count, "visible": visible}
    return results


def _report(label: str, results: dict):
    found = [k for k, v in results.items() if v["count"] > 0]
    missing = [k for k, v in results.items() if v["count"] == 0]
    print(f"\n{'='*60}")
    print(f"  {label}")
    print(f"{'='*60}")
    print(f"  Found ({len(found)}): {', '.join(found) or '(none)'}")
    print(f"  Missing ({len(missing)}): {', '.join(missing) or '(none)'}")
    for k, v in results.items():
        flag = "✓" if v["count"] > 0 else "✗"
        print(f"    {flag} {k}: count={v['count']}, visible={v['visible']}")


def test_onboarding_elements(persistent_app):
    """Check what elements XCUITest finds on the onboarding screen."""
    app = persistent_app
    app.driver.reset()
    time.sleep(2)
    results = _check_elements(app.driver, ONBOARDING_ELEMENTS)
    _report("ONBOARDING (after reset)", results)
    found = sum(1 for v in results.values() if v["count"] > 0)
    assert found > 0, f"XCUITest found 0 elements on onboarding screen: {results}"


def test_sidebar_elements(persistent_app, nest_instance, test_user):
    """Check what elements XCUITest finds in the sidebar."""
    app = persistent_app
    secret_hex = test_user["signing_key"].encode().hex()
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest_instance["url"],
            "secret_hex": secret_hex,
            "handle": "e2e-diag",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "test-device-diag",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    time.sleep(2)
    results = _check_elements(app.driver, SIDEBAR_ELEMENTS)
    _report("SIDEBAR (after login)", results)
    found = sum(1 for v in results.values() if v["count"] > 0)
    assert found > 0, f"XCUITest found 0 sidebar elements: {results}"


def test_feed_elements(persistent_app, nest_instance, test_user):
    """Check what elements XCUITest finds on the feed page."""
    app = persistent_app
    secret_hex = test_user["signing_key"].encode().hex()
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest_instance["url"],
            "secret_hex": secret_hex,
            "handle": "e2e-diag",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "test-device-diag",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    # Feed needs time: login → FaunaClient init → API loadFeeds → auto-select
    time.sleep(5)

    # Dump state to understand what the app sees
    state = app.driver.get_state()
    if state:
        import json
        nav = state.get("nav", {})
        data = state.get("data", {})
        session = state.get("session", {})
        print(f"\n  STATE DUMP:")
        print(f"    session.authenticated: {session.get('authenticated')}")
        print(f"    session.node_url: {session.get('node_url')}")
        print(f"    nav: {json.dumps(nav, indent=6)}")
        feeds = data.get("feeds", [])
        print(f"    data.feeds: {len(feeds)} feeds")
        for f in feeds[:3]:
            print(f"      - {f.get('id', '?')}: {f.get('name', '?')}")

    results = _check_elements(app.driver, FEED_ELEMENTS)
    _report("FEED PAGE", results)


def test_contacts_elements(persistent_app, nest_instance, test_user):
    """Check what elements XCUITest finds on the contacts page."""
    app = persistent_app
    secret_hex = test_user["signing_key"].encode().hex()
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest_instance["url"],
            "secret_hex": secret_hex,
            "handle": "e2e-diag",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "test-device-diag",
        },
        "nav": {"stack": [{"view": "contacts"}]},
    })
    time.sleep(2)
    results = _check_elements(app.driver, CONTACTS_ELEMENTS)
    _report("CONTACTS PAGE", results)
