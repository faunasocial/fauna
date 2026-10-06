"""Bridge-feed subscribe UI and `feed-delete-button` — the positive, end-to-end companion
to `test_feed_bridge_selector_gating.py`'s hidden-when-no-bridges direction.

Positive coverage for `bridge-feed-subscribe-toggle` / `bridge-form-*` /
`bridge-feed-unsubscribe-button` needs the nest to report at least one
available bridge (`version-compatibility.md` § Dim 3: a client must not offer
a protocol the nest's build can't serve). The shared e2e nest gates nostr's
`available()` on `any_nsec_deposited` (`bins/fauna-nest/src/nostr/mod.rs:61`),
so this file makes one reachable the same self-serve way `test_nostr.py`'s
link tests do — `app.nostr.link_generate()` — and restores the unlinked state
on teardown (the `test_content_toggles_round_trip` restore discipline), since
`logged_in_app` is a shared session fixture other tests reuse.

tier_3: a real ``fauna-nest`` binary + real `fauna.bridges.feeds.*` /
`fauna.feed.delete` round-trips (`FeedManager::{subscribe_bridge,
unsubscribe_bridge,delete_feed}`), not an optimistic client-side row removal.
"""

import time
import uuid

import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _feed_item_index(app, name: str) -> int:
    """The rendered `feed-item` text is decorated (an icon prefix, a trailing
    delete glyph on web — e.g. ``'📡 my-feed ✕'``), so this must contain
    ``name`` rather than equal it — the same substring check `open_feed()`
    already uses, for the same reason."""
    count = app.driver.count("feed-item")
    for i in range(count):
        if name in (app.driver.get_text("feed-item", index=i) or ""):
            return i
    return -1


@pytest.mark.feature("custom-feeds")
def test_feed_delete_button_removes_the_feed(logged_in_app):
    """`feed-delete-button` deletes a custom feed with no
    confirmation step — the uniform pattern linux/web/android/windows all
    share (corrects the earlier "apps confirm" claim)."""
    app = logged_in_app
    app.driver.wait_for("feed-view")
    name = _unique("row32-delete-me")
    app.feed.create_feed_with_rule(name, "BodyContains", "row32-tag")

    # `create_feed_with_rule`'s own wait is a fixed 1s sleep, and every OTHER
    # caller immediately follows it with `open_feed()`, which deadline-polls
    # up to 10s for the row to appear (convention 14). A bare, non-retrying
    # `_feed_item_index()` read right here raced that same refresh and lost
    # intermittently — deadline-poll it too rather than trust the 1s alone.
    deadline = time.time() + 10.0
    idx = _feed_item_index(app, name)
    while time.time() < deadline and idx < 0:
        time.sleep(0.3)
        idx = _feed_item_index(app, name)
    assert idx >= 0, (
        f"created feed {name!r} should appear as a feed-item; "
        f"error={app.error_text()!r}"
    )

    app.driver.click("feed-delete-button", scope=f"feed-item[{idx}]")

    deadline = time.time() + 10.0
    while time.time() < deadline and _feed_item_index(app, name) >= 0:
        time.sleep(0.2)
    assert _feed_item_index(app, name) == -1, (
        f"deleted feed {name!r} should no longer appear; error={app.error_text()!r}"
    )


@pytest.mark.feature("bridges")
def test_bridge_subscribe_and_unsubscribe_round_trip(logged_in_app):
    """Link a bridge (nostr, self-serve via a generated keypair) so
    `available_bridges` is non-empty → the subscribe toggle appears → open
    the inline form → subscribe → the unsubscribe row appears → unsubscribe
    → it's gone. Restores the nostr link state it changed."""
    app = logged_in_app
    we_linked = False
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page should be reachable from user settings; error={app.error_text()!r}"
    )
    if not app.nostr.is_linked():
        app.nostr.link_generate()
        assert app.nostr.wait_for_linked(), (
            f"generate-link should link the account so a bridge becomes "
            f"available; error={app.nostr.page_error_text()!r}"
        )
        we_linked = True

    try:
        app.driver.navigate_to("feed")
        app.driver.wait_for("feed-view")
        app.driver.wait_for("bridge-feed-subscribe-toggle", timeout=20)
        app.driver.click("bridge-feed-subscribe-toggle")
        app.driver.wait_for("bridge-form-uri-input")
        assert app.driver.is_visible("bridge-form-bridge-select")
        assert app.driver.is_visible("bridge-form-name-input")

        name = _unique("row31-bridge-feed")
        app.driver.select("bridge-form-bridge-select", "nostr")
        app.driver.clear_and_type("bridge-form-uri-input", "nostr://npub1e2etest")
        app.driver.clear_and_type("bridge-form-name-input", name)

        app.driver.wait_until_enabled("bridge-form-subscribe-button")
        app.driver.click("bridge-form-subscribe-button")

        deadline = time.time() + 15.0
        while time.time() < deadline and app.driver.count("bridge-feed-unsubscribe-button") == 0:
            time.sleep(0.3)
        assert app.driver.count("bridge-feed-unsubscribe-button") > 0, (
            f"a bridge-feed-unsubscribe-button row should appear after "
            f"subscribing; error={app.error_text()!r}"
        )

        app.driver.click("bridge-feed-unsubscribe-button", index=0)
        deadline = time.time() + 15.0
        while time.time() < deadline and app.driver.count("bridge-feed-unsubscribe-button") > 0:
            time.sleep(0.3)
        assert app.driver.count("bridge-feed-unsubscribe-button") == 0, (
            f"the row should be gone after unsubscribe; error={app.error_text()!r}"
        )
    finally:
        if we_linked:
            app.nostr.navigate()
            app.nostr.unlink()
            app.nostr.wait_for_unlinked()
