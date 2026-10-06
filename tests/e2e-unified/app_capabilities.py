"""Per-app capability declarations for state protocol data sections.

Each app declares which data.* sections it populates with real data.
True = app serializes this section (null from get_state is a bug).
False = app hasn't implemented this section yet (null is expected).

Update this dict when an app lands its Phase 1 state serialization work.
The test framework uses this to decide skip vs fail when state data is null.

(design decision, tracked internally)
"""

# Which data sections each app populates with real data.
# True = implemented (null → fail), False = not yet (null → skip).
CAPABILITIES: dict[str, dict[str, bool]] = {
    "web": {
        "feed": True,       # posts with post_id, author, body (cached after WASM decode), tags, has_media
        "contacts": True,    # peer_id, status
        "conversations": False,
        "notifications": False,  # returns null (not implemented)
        "events": False,     # returns null (not implemented)
        "knocks": False,
        "sync": False,       # returns null (not implemented)
    },
    "windows": {
        "feed": True,        # Phase 1 Task 1.1: AppDataSnapshot serializes real posts after decode
        "contacts": False,
        "conversations": False,
        "notifications": True,   # unread_count from AppDataSnapshot
        "events": False,
        "knocks": False,
        "sync": False,
    },
    "linux": {
        "feed": True,        # post_id, author, body, timestamp, tags, has_media, is_reply
        "contacts": False,   # not stored in AppState, serialized as null
        # data.conversations is null — the legacy fauna-native inbox HTTP drain
        # that fed it was removed (WS-RPC-everywhere rip-out). Conversation data
        # now lives in data.conversation_threads (off the shared
        # ConversationsManager snapshot), matching Apple (ios/macos).
        "conversations": False,
        "notifications": False,  # serialized as null
        "events": False,     # not stored in AppState, serialized as null
        "knocks": False,
        "sync": False,
    },
    "ios": {
        "feed": True,        # Phase 1 done: lastFeedPosts pushed from FeedVM callback
        "contacts": False,   # not serialized from ModelContext yet (returns null)
        "conversations": True,  # from ModelContext
        "notifications": False,
        "events": False,
        "knocks": False,
        "sync": False,
    },
    "macos": {
        "feed": True,        # Phase 1 done: FeedVM.lastLoadedPosts + off-MainActor serialization
        "contacts": True,    # from ModelContext (shared with iOS via FaunaKit)
        "conversations": True,
        "notifications": False,
        "events": False,
        "knocks": False,
        "sync": False,
    },
    # tui was MISSING from this dict until 2026-07-29, and the omission was
    # silent in the worst way: `app_name()` matched on driver-class-name
    # substrings, so `TuiDriver` fell through to "unknown", `has_capability()`
    # then answered False for every section, and every state check on tui
    # skipped with "unknown has not implemented …". The null-means-a-bug
    # assertion could never fire for tui at all. Both honesty suites
    # (test_state_ui_honesty.py, test_state_shape_validation.py) carry no app
    # marker, so they collect for tui and were quietly buying nothing.
    # Values below read off `apps/fauna-tui/src/automation.rs`'s `"data"` block.
    "tui": {
        "feed": True,        # feed::state_json — {"posts": [...]}, never null
        "contacts": True,    # contacts::state_json — peer_id/status/handle
        # Like linux + apple: conversation data lives in
        # data.conversation_threads, so data.conversations is null by design.
        "conversations": False,
        "notifications": True,   # notifications::state_json — {"unread_count": N}
        "events": True,      # events::state_json — id/summary/start/end/rsvp_status
        "knocks": False,     # not serialized
        "sync": False,       # not serialized
    },
    "android": {
        "feed": False,       # posts array always empty regardless of real feed state — pre-existing gap, out of this pass's scope
        "contacts": True,    # peer_id, status, handle, node_url from ContactDao (2026-07-19)
        "conversations": False,
        "notifications": False,  # unread_count hardcoded 0 — NotificationsVM is a per-screen Hilt VM, not a singleton TestAgent can read
        "events": False,     # events array always empty — no Room-backed or singleton events cache exists yet
        "knocks": True,      # sender, sender_node, summary, timestamp from KnockDao (2026-07-19)
        "sync": True,        # files: path, folder, size_bytes, state from SyncFileDao (2026-07-19)
    },
}

# Canonical field names for each data section.
# Used by state shape validation to catch field name mismatches
# (e.g., iOS returning "author_id" instead of "author").
EXPECTED_FIELDS: dict[str, set[str]] = {
    "feed.posts": {"post_id", "author", "body", "timestamp", "tags", "has_media", "is_reply"},
    "contacts": {"peer_id", "status", "handle", "node_url"},
    "conversations": {"post_id", "from", "to", "subject", "body", "timestamp", "encrypted", "read", "attachments"},
    "events": {"id", "summary", "start", "end", "location", "description", "rsvp_status"},
    "notifications": {"unread_count"},
    "knocks": {"sender", "sender_node", "summary", "timestamp"},
    "sync.files": {"path", "folder", "size_bytes", "state"},
}


def app_name(driver) -> str:
    """Extract app name from driver for capability lookup.

    Delegates to the single canonical resolver in `helpers/app_surface.py`,
    which asks the driver's own `is_*()` predicates instead of matching
    substrings of its class name. The substring form silently answered
    "unknown" for `TuiDriver` — no substring of "tuidriver" matched any arm —
    which turned every tui capability lookup into a skip and made the
    "declares a capability but returned null" assertion unreachable for that
    app. A new app's driver joins by answering its predicate, not by someone
    remembering to add an arm here.
    """
    from helpers.app_surface import app_name as _canonical_app_name

    return _canonical_app_name(driver)


def has_capability(driver, section: str) -> bool:
    """Check if an app declares a data section as implemented.

    Args:
        driver: PlatformDriver instance
        section: data section name (e.g., "feed", "contacts")

    Returns:
        True if the app declares this section as implemented.
    """
    name = app_name(driver)
    caps = CAPABILITIES.get(name, {})
    return caps.get(section, False)


def can_read_ui_for_honesty(driver) -> bool:
    """Check if this app can reliably read UI elements for honesty checks.

    Returns False for apps where UI element reads crash the bridge or
    return garbage due to accessibility limitations. On these apps,
    honesty checks verify state consistency only (no state-vs-UI comparison).

    macOS: detail pane invisible to XCUITest, navigation to some pages
           crashes the bridge. State-only checks until detail pane is solved.
    android: no E2E bridge yet.
    """
    name = app_name(driver)
    return name not in ("macos", "android")


def check_state_section(driver, state: dict | None, section: str):
    """Check a state data section and return (data, skip_reason).

    Returns:
        (data, None) if section has data — proceed with assertions.
        (None, reason) if section should be skipped — call pytest.skip(reason).

    Raises:
        AssertionError if the app declares capability but returns null.

    Usage:
        data, skip = check_state_section(driver, state, "feed")
        if skip:
            pytest.skip(skip)
        posts = data.get("posts", [])
    """
    if state is None:
        return None, "No state available from app"

    data_section = state.get("data")
    if data_section is None:
        return None, "App returned no data section"

    value = data_section.get(section)
    name = app_name(driver)

    if value is None:
        if has_capability(driver, section):
            raise AssertionError(
                f"{name} declares '{section}' capability but returned null. "
                f"Either fix the serializer or set CAPABILITIES['{name}']['{section}'] = False"
            )
        return None, f"{name} has not implemented {section} serialization"

    return value, None
