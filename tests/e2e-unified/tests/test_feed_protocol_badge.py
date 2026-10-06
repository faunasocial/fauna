"""Cross-app Feed — the ``protocol-badge`` (federation source indicator).

``tests/e2e-unified/ui.yaml`` ``protocol-badge`` (inside ``post-card``);
``docs/goal/architecture/render-model.md`` § Deltas (D5 — the shared
``SourceGlyph`` map every app's badge and the conversations rail both read,
so they can't drift). One badge per classified source token in the wire
``source`` field, via the shared ``fauna_feed::classify_sources`` (linux
``build_protocol_badges`` / web ``wasm.classifySources``) — unknown/empty
tokens yield zero badges, known tokens (``fauna``/``bluesky``/``nostr``/
``activitypub``/``email``) yield one badge each, deduplicated.

tier_2: seeded via ``feed_inject_posts`` (the same seam
``test_feed_unverified_source.py`` uses) rather than a live nest round-trip —
a real post's ``source`` is nest-classified server-side and not otherwise
controllable on demand.

android: ``ProtocolBadge.kt`` renders with the exact ``protocol-badge`` testTag,
wired into ``FeedScreen``'s ``post-card`` composable;its render leg is untested on this dev VM for lack
of a device — the marker is the parse-only parity contract, execution awaits
the standing android-e2e gap every app-gated row carries, same as every other
android e2e mark.
"""

import pytest

pytestmark = [pytest.mark.tier_2]

_POSTS = [
    {
        "post_id": "a" * 64,
        "author": "1" * 64,
        "body": "a native post",
        "source": "",
    },
    {
        "post_id": "b" * 64,
        "author": "2" * 64,
        "body": "bridged from bluesky",
        "source": "bluesky",
    },
    {
        "post_id": "c" * 64,
        "author": "3" * 64,
        "body": "bridged from two protocols",
        "source": "bluesky,nostr",
    },
]


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("post-badges")
def test_protocol_badge_count_matches_classified_sources(logged_in_app):
    """``protocol-badge`` renders once per classified source token: zero for an
    empty ``source`` field, one for a single known token, two for two distinct
    tokens (deduplicated, per ``classify_sources``)."""
    app = logged_in_app
    n = app.feed.seed_posts(_POSTS)
    assert n == len(_POSTS), (
        f"expected {len(_POSTS)} injected post-cards, got {n}; "
        f"error={app.error_text()!r}"
    )

    assert app.driver.count("protocol-badge", scope="post-card[0]") == 0, (
        "a post with an empty source field should show no protocol-badge; "
        f"{app.driver.diagnose('protocol-badge')}"
    )
    assert app.driver.count("protocol-badge", scope="post-card[1]") == 1, (
        "a post bridged from a single known source should show exactly one "
        f"protocol-badge; {app.driver.diagnose('protocol-badge')}"
    )
    assert app.driver.count("protocol-badge", scope="post-card[2]") == 2, (
        "a post bridged from two distinct sources should show two "
        f"protocol-badges (one per classified token); "
        f"{app.driver.diagnose('protocol-badge')}"
    )
