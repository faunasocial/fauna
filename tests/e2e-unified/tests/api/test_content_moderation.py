"""E2E test: the post label plane — attach → feed filter → moderation API.

The nest classifies nothing at ingest (``storage-modes.md`` § Implementation
status today; ``content-scoring.md`` § The placement matrix): a content scorer
runs only at a capability position — the user's client post-decrypt, or a
granted content-processing holder (``test_capability_rescore_drain.py`` /
``test_capability_labeler_drain.py`` prove that arm for mail). This test pins
the surviving nest-side label plane those positions write into and every feed
read consumes:

1. Bob posts a clean message; Charlie posts obvious spam (both signed —
   ``bare.sign_and_encode_post`` is the only e2e seeding path).
2. Charlie's own scoring position attaches a spam label to Charlie's post over
   the real wire (``fauna.labels.attach``), and Alice's attempt to label it —
   someone else's post, no grant — is refused. The door admits the post's author
   or a holder of that author's ``content.label-write`` grant, and nobody else
   (``moderation.md`` § Per-row badge data path → *The attach door enforces
   that producer set*): the rows it writes feed two verdicts that bind every
   reader, so a class-only gate made any member a nest-wide remover of any
   member's post.
3. Alice's LabelBelow-filtered feed shows Bob only.
4. The local discovery feed's default spam suppression (``ensure_spam_filter``,
   LabelBelow spam 0.5) hides Charlie's post.
5. ``fauna.moderation.stats`` reports the spam label.
6. ``fauna.moderation.actions`` succeeds (empty without obligation rules).
7. A LabelAbove junk feed shows Charlie only.
8. ``fauna.nest.info`` carries the moderation field.

The attach in step 2 is the mutation under test — an API-contract exercise of
the label plane (testing-rules carve-out (a)), NOT a stand-in for retired
nest-side classification. It is issued by the post's AUTHOR because that is one
of the two positions the door admits; a third party's verdict needs the author's
grant, whose own arm is ``test_capability_labeler_drain.py``. Ingest-time classification is deliberately absent
from this fixture and must not be reintroduced (the pre-2026-07-13 version of
this test pinned that retired arm and sat red from the moment the demolition
landed).

Posts, feeds, labels, moderation reads, and node-info are all driven over
WS-RPC (``fauna.posts.*`` / ``fauna.feed.*`` / ``fauna.labels.attach`` /
``fauna.moderation.{stats,actions}`` / ``fauna.nest.info``) — every one of the
corresponding HTTP twins was deleted by the WS-RPC-everywhere migration; no
moderation route stays HTTP.
"""

import time

from clients.ws_rpc_admin_client import RpcCallError
from common import create_actor_and_register
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

import pytest

pytestmark = pytest.mark.tier_3


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

@pytest.mark.feature("moderation-queue", "post-badges")
def test_spam_filtering_pipeline(two_nodes):
    """Label plane end-to-end: attach → feed filter → stats → junk feed."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    # --- Register users ---
    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    charlie = create_actor_and_register(port, admin_signing_key=admin_sk)

    # --- Bob posts a clean message ---
    now_us = int(time.time() * 1_000_000)
    clean_text = "Beautiful day for hiking in the mountains. The views are stunning!"
    clean_post = sign_and_encode_post(bob["signing_key"], now_us, clean_text, tags=[])
    clean_post_id = ws_api.create_post(port, bob, clean_post)

    # --- Charlie posts spam ---
    spam_text = (
        "BUY NOW!!! FREE MONEY!!! CLICK HERE for amazing deals!!! "
        "Limited time offer!!! Act now!!! "
        "https://spam1.example.com https://spam2.example.com "
        "https://spam3.example.com https://spam4.example.com"
    )
    spam_post = sign_and_encode_post(charlie["signing_key"], now_us + 1_000_000, spam_text, tags=[])
    spam_post_id = ws_api.create_post(port, charlie, spam_post)

    # --- Alice may NOT label Charlie's post ---
    # The door is an ownership gate, not a class gate: the rows it writes feed
    # the mandatory feed spam group and the nest-as-publisher fold, both of which
    # bind every reader, so a member's verdict about another member's post would
    # be a nest-wide removal on one member's opinion — what `moderation.md`
    # § Categories & enforcement item 1 forbids the nest to do at all.
    with pytest.raises(RpcCallError) as refused:
        ws_api.attach_labels(
            port, alice, spam_post_id,
            [{"category": "spam", "confidence_per_mille": 900}],
        )
    assert refused.value.code == "fauna.labels.permission_denied", (
        f"a third party's label must be refused, got {refused.value.code}"
    )

    # --- Charlie's own scoring position labels Charlie's post spam ---
    # In production this attach is issued by a position the author's own key
    # reaches (their client post-decrypt) or by a holder of their
    # `content.label-write` grant; the nest never computes the verdict itself.
    # 900‰ clears both the explicit 300‰ rules below and the local feed's
    # default 500‰ suppression.
    stored = ws_api.attach_labels(
        port, charlie, spam_post_id,
        [{"category": "spam", "confidence_per_mille": 900}],
    )
    assert stored == 1, "the spam label should be stored"

    # --- 1. Alice creates a spam-filtered feed ---
    inbox_feed_id = ws_api.create_feed(port, alice, "Inbox (no spam)", rules=[
        {"LabelBelow": {"category": "spam", "max_confidence_permille": 300}},
    ])

    # --- 2. Query the filtered feed: only Bob's clean post ---
    posts = ws_api.feed_posts(port, alice, inbox_feed_id)
    post_ids = [p["post_id"] for p in posts]
    assert clean_post_id in post_ids, "Bob's clean post should be in filtered feed"
    assert spam_post_id not in post_ids, "Charlie's spam should NOT be in filtered feed"

    # --- 3. Query the local discovery feed: clean post present ---
    # The local feed applies a default spam-suppression rule
    # (`ensure_spam_filter` → LabelBelow spam 0.5, added), so
    # Charlie's labeled spam is filtered out here by design. Its presence in
    # the system (ingested + labeled) is verified by the moderation stats
    # (step 4) and the junk feed (step 6, LabelAbove).
    local_posts = ws_api.local_feed_posts(port, alice)
    local_ids = [p["post_id"] for p in local_posts]
    assert clean_post_id in local_ids, "Bob's post should be in local feed"
    assert spam_post_id not in local_ids, \
        "Charlie's spam is suppressed by the local feed's default spam filter"

    # --- 4. Moderation stats show spam labels ---
    # fauna.moderation.stats replies with a labels LIST (the HTTP twin's
    # category-keyed dict is gone); find the spam category by `category`.
    labels = ws_api.moderation_stats(port, alice)
    spam = next((l for l in labels if l["category"] == "spam"), None)
    assert spam is not None, f"expected a 'spam' entry in moderation stats: {labels}"
    assert spam["count"] >= 1, "expected at least 1 spam label"

    # --- 5. Charlie queries his moderation actions ---
    # The WS kind scopes to the connection actor, so Charlie reads his own
    # records (the twin's `?actor=` any-actor query is dropped). Actions are only
    # recorded when obligation rules are configured; without them this is an
    # empty list (expected). We just assert the call succeeds.
    ws_api.moderation_actions(port, charlie)

    # --- 6. Alice creates a junk/spam feed (LabelAbove) ---
    junk_feed_id = ws_api.create_feed(port, alice, "Spam folder", rules=[
        {"LabelAbove": {"category": "spam", "min_confidence_permille": 300}},
    ])

    junk_posts = ws_api.feed_posts(port, alice, junk_feed_id)
    junk_ids = [p["post_id"] for p in junk_posts]
    assert spam_post_id in junk_ids, "Charlie's spam should be in junk feed"
    assert clean_post_id not in junk_ids, "Bob's clean post should NOT be in junk feed"

    # --- 7. Node-info includes moderation field ---
    info = ws_api.nest_info(port)
    assert "moderation" in info, "node-info should include moderation field"

    # --- 8. Bob posts another clean message — no labels ---
    clean2_text = "Just finished a great book. Highly recommend it to everyone."
    clean2_post = sign_and_encode_post(bob["signing_key"], now_us + 2_000_000, clean2_text, tags=[])
    clean2_post_id = ws_api.create_post(port, bob, clean2_post)

    # Both clean posts in filtered feed, spam still excluded
    filtered = ws_api.feed_posts(port, alice, inbox_feed_id)
    filtered_ids = [p["post_id"] for p in filtered]
    assert clean_post_id in filtered_ids
    assert clean2_post_id in filtered_ids
    assert spam_post_id not in filtered_ids
