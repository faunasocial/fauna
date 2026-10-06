"""E2E API test: the local-feed body search (``search`` → ``BodyContains``).

The UI-side twin is ``test_feed_search.py::test_feed_search_filters_posts``
(every app's feed search box debounces into a re-query with ``search`` —
``feed.md`` § Where logic lives: search is a re-query, never a client-side
filter). This nest-direct test exists per cross-app convention point 5:
that UI test's failure signature ("count never narrows") cannot distinguish a
client-glue break from a nest-side one — this pins the nest half of the
contract, including the hyphenated-needle shape the UI test actually sends
(``needle-<hex>`` must survive FTS5 quoting as a phrase, not an operator).
"""

import time
import uuid

import pytest
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("feed-search")
def test_local_feed_search_narrows_by_body(two_nodes):
    """``fauna.feed.local.posts`` with ``search`` returns only matching posts."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)

    # The SAME needle shape the UI test seeds (hyphenated uuid fragment).
    needle = f"needle-{uuid.uuid4().hex[:8]}"
    haystack = f"haystack-{uuid.uuid4().hex[:8]}"
    now_us = int(time.time() * 1_000_000)

    needle_post = sign_and_encode_post(
        author["signing_key"], now_us, f"Finding the {needle} in posts")
    needle_id = ws_api.create_post(port, author, needle_post)
    haystack_post = sign_and_encode_post(
        author["signing_key"], now_us + 1_000_000, f"This is just {haystack}")
    ws_api.create_post(port, author, haystack_post)

    # Unfiltered baseline: both posts are served.
    unfiltered = ws_api.local_feed_posts(port, author)
    assert len(unfiltered) == 2, (
        f"expected both seeded posts unfiltered, got {len(unfiltered)}: {unfiltered}"
    )

    # The search MUST narrow to exactly the needle post. Returning both posts
    # means the filter never applied; returning zero means the post was never
    # FTS-indexed (or the hyphenated term broke the MATCH expression).
    filtered = ws_api.local_feed_posts(port, author, search=needle)
    assert len(filtered) == 1, (
        f"search={needle!r} should return exactly the needle post, got "
        f"{len(filtered)}: {[p.get('body') for p in filtered]}"
    )
    assert filtered[0]["post_id"] == needle_id

    # A no-match term returns an empty page, not the unfiltered feed.
    none = ws_api.local_feed_posts(port, author, search="zzz-nonexistent-zzz")
    assert none == [], f"no-match search must return [], got {none}"


@pytest.mark.feature("feed-search")
def test_custom_any_feed_search_narrows_by_body(two_nodes):
    """Search must narrow a CUSTOM feed regardless of its rule combination.

    The exact shape every apple e2e login sees: the conftest's default
    "General" feed is empty-rules + ``combination: "any"``, and the apple
    apps auto-select the first feed — so the feed-search re-query rides
    ``fauna.feed.posts``. Search (like the spam guard) is a MANDATORY
    constraint: pushed into the feed's own rule set under an ``Any``
    combination it becomes just another OR-alternative and can never narrow
    (the month-old ``test_feed_search_filters_posts`` apple red).
    """
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)

    needle = f"needle-{uuid.uuid4().hex[:8]}"
    now_us = int(time.time() * 1_000_000)
    needle_post = sign_and_encode_post(
        author["signing_key"], now_us, f"Finding the {needle} in posts")
    needle_id = ws_api.create_post(port, author, needle_post)
    haystack_post = sign_and_encode_post(
        author["signing_key"], now_us + 1_000_000, "This is just hay")
    ws_api.create_post(port, author, haystack_post)

    feed_id = ws_api.create_feed(port, author, "General", rules=[], combination="any")

    unfiltered = ws_api.feed_posts(port, author, feed_id)
    assert len(unfiltered) == 2, (
        f"catch-all Any feed should serve both posts, got {len(unfiltered)}"
    )

    filtered = ws_api.feed_posts(port, author, feed_id, search=needle)
    assert len(filtered) == 1, (
        f"search={needle!r} on the Any feed must narrow to the needle post, got "
        f"{len(filtered)}: {[p.get('body') for p in filtered]}"
    )
    assert filtered[0]["post_id"] == needle_id

    none = ws_api.feed_posts(port, author, feed_id, search="zzz-nonexistent-zzz")
    assert none == [], f"no-match search on the Any feed must return [], got {none}"
