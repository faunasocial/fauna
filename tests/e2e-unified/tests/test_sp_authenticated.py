"""State-protocol tests: authenticated user flow.

One app launch, one login via set_state(), then exercise all features
that require an authenticated session. Tests run in file order and
share the app instance.
"""
import uuid
import time
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_3

FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.fixture(scope="module")
def app(persistent_app, nest_instance, test_user, web_api_url):
    """Log in once for the entire module."""
    secret_hex = test_user["signing_key"].encode().hex()
    node_url = web_api_url if persistent_app.driver.is_web() else nest_instance["url"]
    persistent_app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": secret_hex,
            "handle": "e2e-sp-user",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "test-device-sp",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    return persistent_app


# ---------------------------------------------------------------------------
# Navigation
# ---------------------------------------------------------------------------

# "groups" is intentionally absent: it is not a canonical page in ui.yaml
# (no `groups` in pages/tabs) — groups were merged into `conversations` via the
# ConversationsManager convergence.
# "devices" is intentionally absent too: the 2026-06-28 sync/folder UI
# unification split it into a Settings sub-page on every migrated app
# (`actions/backups.py::_folders_in_settings()`), so the bare {"view":
# "devices"} nav this test used no longer round-trips anywhere migrated —
# same class of drift already retired from test_navigation.py's
# PAGES list. Dedicated devices/folders tests already cover the page via the
# app-aware navigate_devices() helper.
@pytest.mark.parametrize("page", [
    "conversations", "events", "contacts",
    "media", "backups", "settings",
])
def test_navigate_to_page(app, page):
    """Navigate to each page via set_state and verify the view loads."""
    app.driver.set_state({"nav": {"stack": [{"view": page}]}})
    state = app.driver.get_state()
    assert state is not None, f"get_state() returned None after navigating to {page!r}"
    assert state["nav"]["stack"][0]["view"] == page, (
        f"nav stack should top out at {page!r} after set_state; got nav={state.get('nav')!r}"
    )


# ---------------------------------------------------------------------------
# Feed
# ---------------------------------------------------------------------------

@pytest.mark.feature("feed-read")
def test_feed_visible(app):
    """Feed page loads after login."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view", timeout=10)


@pytest.mark.tier2
@pytest.mark.feature("feed-compose")
def test_create_post(app):
    """Create a post and verify it appears.

    Requires a working nest connection (feed definitions must load).
    On iOS the compose field only appears after a feed is selected.
    """
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view", timeout=10)
    text = _unique("sp-post")
    app.feed.create_post(text=text)
    assert app.feed.first_post_text() == text, (
        f"newly-created post {text!r} should be first in feed; got "
        f"{app.feed.first_post_text()!r}: {app.driver.diagnose('feed-view')} "
        f"error={app.error_text()!r}"
    )


def test_create_multiple_posts(app):
    """Create two posts and verify count increases."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view", timeout=10)
    initial = app.feed.post_count()
    app.feed.create_post(text=_unique("sp-multi-1"))
    app.feed.create_post(text=_unique("sp-multi-2"))
    assert app.feed.post_count() >= initial + 2, (
        f"post count should grow by ≥2 after two creates; initial={initial} "
        f"now={app.feed.post_count()}: error={app.error_text()!r}"
    )


def test_post_ordering_newest_first(app):
    """Post 3 messages, verify newest appears first."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view", timeout=10)
    t1 = _unique("sp-order-1")
    t2 = _unique("sp-order-2")
    t3 = _unique("sp-order-3")
    app.feed.create_post(text=t1)
    app.feed.create_post(text=t2)
    app.feed.create_post(text=t3)
    time.sleep(1)
    assert app.feed.post_text(0) == t3, (
        f"newest post {t3!r} should be first; got {app.feed.post_text(0)!r}: "
        f"{app.driver.diagnose('feed-view')}"
    )


def test_post_with_tags(app):
    """Post with tags, verify tag chips render."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view", timeout=10)
    text = _unique("sp-tagged")
    before = app.feed.tag_chip_count()
    app.feed.create_post_with_tags(text=text, tags="rust, svelte, wasm")
    time.sleep(1)
    tags = app.feed.post_tags_since(before)
    assert len(tags) == 3, (
        f"posting with 3 tags should yield 3 tag chips; got {len(tags)}: {tags!r} "
        f"error={app.error_text()!r}"
    )
    assert "#rust" in tags, f"#rust should be among the rendered tag chips; got {tags!r}"


# ---------------------------------------------------------------------------
# Messaging
# ---------------------------------------------------------------------------

def test_conversations_page_loads(app):
    """Navigate to conversations and verify UI."""
    app.driver.set_state({"nav": {"stack": [{"view": "conversations"}]}})
    # Canonical new-conversation-button (conversations.md); the
    # conversations-list identifier on NSSplitViewController may not be
    # hittable, so fall back to the sidebar tab as proof the page loaded.
    assert (app.driver.is_visible("new-conversation-button")
            or app.driver.is_visible("conversations-list")
            or app.driver.is_visible("conversations-tab")), (
        "conversations page should prove it loaded via the new-conversation-button, "
        "the conversations-list, or the conversations-tab: "
        f"new-conversation-button={app.driver.diagnose('new-conversation-button')} "
        f"conversations-list={app.driver.diagnose('conversations-list')} "
        f"conversations-tab={app.driver.diagnose('conversations-tab')}"
    )


def test_conversation_search_visible(app):
    """Verify conversation search box is visible."""
    app.driver.set_state({"nav": {"stack": [{"view": "conversations"}]}})
    assert app.driver.is_visible("conversation-search-box"), (
        "conversations page should show the search box: "
        f"{app.driver.diagnose('conversation-search-box')}"
    )


def test_markdown_toolbar_visible(app):
    """Verify markdown toolbar is present.

    The toolbar lives inside the compose bar, revealed by the canonical
    new-conversation-button (in-pane new-thread compose, per conversations.md).
    """
    app.driver.set_state({"nav": {"stack": [{"view": "conversations"}]}})
    app.driver.wait_for("new-conversation-button")
    app.driver.click("new-conversation-button")
    assert app.driver.is_visible("markdown-bold-button"), (
        "the compose bar should render the markdown toolbar (bold button): "
        f"{app.driver.diagnose('markdown-bold-button')}"
    )


# ---------------------------------------------------------------------------
# Contacts
# ---------------------------------------------------------------------------

def test_contacts_visible(app):
    """Navigate to contacts and verify list loads."""
    app.driver.set_state({"nav": {"stack": [{"view": "contacts"}]}})
    assert app.contacts.contact_count() >= 0, (
        "contact roster should be queryable (count never negative): "
        f"{app.driver.diagnose('contacts-view')}"
    )


def test_actor_id_field_visible(app):
    """Verify actor ID lookup field on contacts page."""
    app.driver.set_state({"nav": {"stack": [{"view": "contacts"}]}})
    assert app.driver.is_visible("contact-actor-id-field"), (
        "contacts page should show the actor-id lookup field: "
        f"{app.driver.diagnose('contact-actor-id-field')}"
    )
    assert app.driver.is_visible("contact-actor-id-lookup"), (
        "contacts page should show the actor-id lookup button: "
        f"{app.driver.diagnose('contact-actor-id-lookup')}"
    )


# ---------------------------------------------------------------------------
# Events
# ---------------------------------------------------------------------------

def test_calendar_view_toggles(app):
    """Verify calendar view toggles are visible."""
    app.driver.set_state({"nav": {"stack": [{"view": "events"}]}})
    assert app.driver.is_visible("calendar-view-agenda"), (
        "events page should show the agenda view toggle: "
        f"{app.driver.diagnose('calendar-view-agenda')}"
    )
    assert app.driver.is_visible("calendar-view-month"), (
        "events page should show the month view toggle: "
        f"{app.driver.diagnose('calendar-view-month')}"
    )
    assert app.driver.is_visible("calendar-view-week"), (
        "events page should show the week view toggle: "
        f"{app.driver.diagnose('calendar-view-week')}"
    )
    assert app.driver.is_visible("calendar-view-day"), (
        "events page should show the day view toggle: "
        f"{app.driver.diagnose('calendar-view-day')}"
    )


# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

def test_settings_page_loads(app):
    """Navigate to settings and verify it loaded."""
    app.driver.set_state({"nav": {"stack": [{"view": "settings"}]}})
    state = app.driver.get_state()
    assert state["nav"]["stack"][0]["view"] == "settings", (
        f"nav stack should top out at 'settings'; got nav={state.get('nav')!r}"
    )
    if not app.driver.is_ios():
        # account-settings-link is the ui.yaml-documented canonical settings
        # landmark (settings.elements: "navigate to account settings / page
        # landmark") — the one every app renders on the settings landing
        # (tui's Root rail row included); spam-preferences / settings-view
        # remain accepted for clients that land on those surfaces instead.
        assert (
            app.driver.is_visible("spam-preferences")
            or app.driver.is_visible("settings-view")
            or app.driver.is_visible("account-settings-link")
        ), (
            "settings page should render spam-preferences, the settings-view "
            "container, or the account-settings-link landmark: "
            f"spam-preferences={app.driver.diagnose('spam-preferences')} "
            f"settings-view={app.driver.diagnose('settings-view')} "
            f"account-settings-link={app.driver.diagnose('account-settings-link')}"
        )


# ---------------------------------------------------------------------------
# Notifications
# ---------------------------------------------------------------------------

def test_notifications_page_loads(app):
    """Navigate to notifications and verify UI."""
    app.driver.set_state({"nav": {"stack": [{"view": "notifications"}]}})
    assert app.driver.is_visible("notification-mark-read") or app.driver.is_visible("notification-count-badge"), (
        "notifications page should render the mark-read control or the count badge: "
        f"notification-mark-read={app.driver.diagnose('notification-mark-read')} "
        f"notification-count-badge={app.driver.diagnose('notification-count-badge')}"
    )


# ---------------------------------------------------------------------------
# Backups & Media
# ---------------------------------------------------------------------------

def test_backups_page_loads(app):
    """Navigate to backups page."""
    app.driver.set_state({"nav": {"stack": [{"view": "backups"}]}})
    state = app.driver.get_state()
    assert state["nav"]["stack"][0]["view"] == "backups", (
        f"nav stack should top out at 'backups'; got nav={state.get('nav')!r}"
    )


def test_media_page_loads(app):
    """Navigate to media page."""
    app.driver.set_state({"nav": {"stack": [{"view": "media"}]}})
    state = app.driver.get_state()
    assert state["nav"]["stack"][0]["view"] == "media", (
        f"nav stack should top out at 'media'; got nav={state.get('nav')!r}"
    )


# ---------------------------------------------------------------------------
# Error handling
# ---------------------------------------------------------------------------

def test_error_text_is_readable(app):
    """Verify error_text() returns a string (not crash)."""
    error = app.error_text()
    assert isinstance(error, str), (
        f"error_text() should return a str, not {type(error).__name__}: {error!r}"
    )
