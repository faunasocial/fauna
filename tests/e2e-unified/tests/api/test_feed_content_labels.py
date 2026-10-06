"""E2E test: per-row content-label projection on feed reads.

Proves `moderation.md` § Per-row badge data path: a label attached via
``fauna.labels.attach`` is served back on the matching post's ``labels`` field
by ``fauna.feed.posts`` / ``fauna.feed.local.posts`` — the data path the
``content-label-badge`` renders from. Uses the same ``fauna.feed.*`` /
``fauna.labels.*`` WS-RPC kinds as the sibling ``test_feed_filter.py``.
"""

import time

import pytest
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("post-badges")
def test_attached_label_is_served_per_row_on_feed_reads(two_nodes):
    """Alice's feed includes Bob's post; attaching a label to it surfaces the
    label on the post's `labels` field, and does not leak onto an unlabelled
    sibling post."""
    port_a = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    alice = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port_a, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    labelled_post = sign_and_encode_post(bob["signing_key"], now_us, "a post about mortgages")
    labelled_id = ws_api.create_post(port_a, bob, labelled_post)
    unlabelled_post = sign_and_encode_post(
        bob["signing_key"], now_us + 1_000_000, "an unrelated post"
    )
    unlabelled_id = ws_api.create_post(port_a, bob, unlabelled_post)

    # No label attached yet: neither post carries any.
    feed_id = ws_api.create_feed(port_a, alice, "All", rules=[])
    posts = {p["post_id"]: p for p in ws_api.feed_posts(port_a, alice, feed_id)}
    assert posts[labelled_id].get("labels", []) == []
    assert posts[unlabelled_id].get("labels", []) == []

    # A classifier (here: the API caller standing in for one) attaches two
    # category verdicts to the same post — the highest-confidence row per
    # category is what the feed projection should serve.
    stored = ws_api.attach_labels(
        port_a,
        bob,
        labelled_id,
        [
            {"category": "commercial", "confidence_per_mille": 700},
            {"category": "commercial", "confidence_per_mille": 900},
            {"category": "spam", "confidence_per_mille": 200},
        ],
    )
    assert stored == 3

    posts = {p["post_id"]: p for p in ws_api.feed_posts(port_a, alice, feed_id)}
    labelled_labels = {
        (entry["category"], entry["confidence_per_mille"])
        for entry in posts[labelled_id]["labels"]
    }
    assert labelled_labels == {("commercial", 900), ("spam", 200)}
    # The unlabelled sibling post stays unaffected — no cross-post leakage.
    assert posts[unlabelled_id].get("labels", []) == []
