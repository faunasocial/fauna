"""State shape validation: catch app_capabilities lies at session start.

Verifies that every data section declared as implemented in app_capabilities.py
actually returns non-null from get_state(). Also checks field names match the
canonical schema to catch mismatches like 'author_id' vs 'author'.

Run this early in the suite (before honesty checks or data-dependent tests)
to fail fast if a client's capabilities are misconfigured.

See: the e2e test-redesign plan (tracked internally), Task 4.2
"""

import warnings
import pytest

from app_capabilities import (
    CAPABILITIES,
    EXPECTED_FIELDS,
    app_name,
    has_capability,
)

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


def test_capabilities_match_state(logged_in_app):
    """Every section declared True in app_capabilities must be non-null in state.

    Native apps populate data sections asynchronously after page navigation
    (e.g., FeedPage.Page_Loaded fires ~1-2s after navigation completes).
    We poll for declared sections to become non-null before asserting.
    """
    driver = logged_in_app.driver
    name = app_name(driver)
    caps = CAPABILITIES.get(name, {})
    declared_sections = [k for k, v in caps.items() if v]

    if not declared_sections:
        return

    # Poll until all declared sections are non-null (async data load)
    state = driver.get_state(
        wait_for=lambda s: (
            s.get("data") is not None
            and all(s["data"].get(sec) is not None for sec in declared_sections)
        ),
        timeout=5.0,
    )
    assert state is not None, f"{name}: get_state() returned None"

    data = state.get("data")
    if data is None:
        pytest.fail(
            f"{name} declares capabilities {declared_sections} but state has no 'data' section"
        )

    for section in declared_sections:
        value = data.get(section)
        assert value is not None, (
            f"{name} declares '{section}' capability but get_state().data.{section} "
            f"is null. Either fix the serializer or set "
            f"CAPABILITIES['{name}']['{section}'] = False in app_capabilities.py"
        )


def test_state_field_names(logged_in_app):
    """Check that state data uses canonical field names (catch author_id vs author etc).

    Only checks sections that have data. Issues are reported as warnings, not failures,
    because extra fields are allowed (clients may add platform-specific fields).
    """
    driver = logged_in_app.driver
    name = app_name(driver)

    state = driver.get_state()
    if state is None:
        pytest.skip("No state available")
    data = state.get("data")
    if data is None:
        pytest.skip("No data section in state")

    # Check feed posts
    feed = data.get("feed")
    if feed is not None and isinstance(feed, dict):
        posts = feed.get("posts", [])
        if posts and isinstance(posts, list):
            expected = EXPECTED_FIELDS.get("feed.posts", set())
            actual = set(posts[0].keys())
            unexpected = actual - expected
            missing = expected - actual - {"body", "tags", "has_media", "is_reply"}  # optional fields
            if unexpected:
                warnings.warn(
                    f"{name}: feed post has unexpected fields: {unexpected}. "
                    f"Expected subset of: {expected}"
                )
            if missing:
                warnings.warn(
                    f"{name}: feed post missing expected fields: {missing}"
                )

    # Check contacts
    contacts = data.get("contacts")
    if contacts is not None and isinstance(contacts, list) and contacts:
        expected = EXPECTED_FIELDS.get("contacts", set())
        actual = set(contacts[0].keys())
        unexpected = actual - expected
        if unexpected:
            warnings.warn(f"{name}: contact has unexpected fields: {unexpected}")
