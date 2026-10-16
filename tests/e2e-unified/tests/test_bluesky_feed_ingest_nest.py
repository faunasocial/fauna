"""What the consume-side Bluesky poller stores, read from outside the nest:
one post once, a repost as its original, a self-label as a content warning.

`docs/goal/behavior/bridges.md` § Unified feed ingestion → *Bridge ingestion*:
the ``bluesky_posts`` map's ``UNIQUE(at_uri)`` is the dedupe — a re-poll of the
same window, or a second linked account whose timeline carries the same post,
adds nothing (ruling 2); a repost item enters as the reposted post itself,
once, under its author (ruling 3); a self-labelled post carries its first label
as ``content_warning`` (ruling 4). The headless halves are pinned in Rust
(``conformance_bluesky_feed_ingest.rs`` for the re-poll, the lib tests in
``fauna_bridge_atproto::ingest`` for the repost and the label); these are the
same three through a running nest, its real OAuth client, poller and store,
with only the atproto network canned (:class:`helpers.atproto_fakes.
FakeAtprotoFarEnd`).

No app is launched: each outcome is the nest's own (`docs/features/atproto.md`
outcomes 44–46), observed over the user's WS-RPC reads — ``fauna.feed.local.
posts`` for what is in the feed and under whom, ``fauna.posts.get`` for the
stored post's ``content_warning`` (the feed row does not carry it). Linking and
the poll poke are the neighbour module's (``test_bluesky_feed_ingest.py``,
whose docstring argues why neither is a shortcut).

tier_3: a locally-built nest; the atproto network is a fake.
"""

from __future__ import annotations

import blake3
import cbor2
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    _link_through_callback,
    _local_bluesky_posts,
    _poll_now,
    atproto_far_end,
    bluesky_feed_nest,
)

pytestmark = [pytest.mark.tier_3]

FOLLOWED_DID = "did:plc:erinfaketestingestwit234"
FOLLOWED_HANDLE = "erin.test"

#: Someone the linked account follows who reposts ``FOLLOWED``'s post.
REPOSTER_DID = "did:plc:frankfaketestreposter234"
REPOSTER_HANDLE = "frank.test"


def _bluesky_actor_hex(did: str) -> str:
    """The synthetic actor a Bluesky author rests under — bridges.md ruling 1:
    ``blake3::derive_key("fauna-bluesky-synthetic", did)``, keyed on the DID."""
    return blake3.blake3(
        did.encode(), derive_key_context="fauna-bluesky-synthetic",
    ).hexdigest()


def _stored_post(nest, post_id: str) -> dict:
    """The stored ``Post`` behind ``post_id`` — ``fauna.posts.get``'s body is
    the post's canonical dag-cbor encoding."""
    user = nest["user"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        reply = client.call("fauna.posts.get", {"post_id": post_id})
    return cbor2.loads(bytes(reply["body"]))


@pytest.mark.feature("atproto")
def test_a_bluesky_post_lands_in_the_feed_once_across_polls_and_linked_accounts(
    bluesky_feed_nest, atproto_far_end,
):
    """Two users each link a Bluesky account whose timeline carries the same
    post; two polls later the post rests once and each user's feed holds it
    once. The second account's pass and the re-poll store nothing."""
    from conftest import _make_user

    nest, far = bluesky_feed_nest, atproto_far_end
    second = _make_user(nest)
    _link_through_callback(nest, far)
    _link_through_callback(nest, far, second)

    text = "a post two linked accounts both see"
    far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE, rkey="3kdedupe1", text=text,
    )
    first = _poll_now(nest)
    assert first["errors"] == [] and first["actors"] == 2, (
        f"the poll should walk both linked accounts: {first!r}; far end saw {far.requests!r}"
    )
    assert first["stored"] == 1, (
        f"two timelines carrying one post store it once, not once per account: {first!r}"
    )
    again = _poll_now(nest)
    assert again["errors"] == [] and again["stored"] == 0, (
        f"a re-poll of the same window stores nothing: {again!r}"
    )

    for who, user in (("the first user", nest["user"]), ("the second user", second)):
        wire = _local_bluesky_posts(nest, text, user)
        assert len(wire) == 1, (
            f"{who}'s fauna.feed.local.posts should carry the post {text!r} exactly "
            f"once after two polls over two linked accounts; got {wire!r}"
        )


@pytest.mark.feature("atproto")
def test_a_bluesky_repost_arrives_as_the_original_post_under_its_author(
    bluesky_feed_nest, atproto_far_end,
):
    """A post the linked account sees only because someone it follows reposted
    it rests as that post, under its original author — never the reposter."""
    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)

    text = "a post that reaches the timeline only as a repost"
    uri = far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE, rkey="3krepost1", text=text,
        on_timeline=False,
    )
    far.repost(uri, by_did=REPOSTER_DID, by_handle=REPOSTER_HANDLE)
    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] == 1, (
        f"the poll should store the reposted post, once: {report!r}; far end saw {far.requests!r}"
    )

    wire = _local_bluesky_posts(nest, text)
    assert len(wire) == 1, (
        f"the repost should arrive as the one original post {text!r}; got {wire!r}"
    )
    author = wire[0]["author"]
    assert author == _bluesky_actor_hex(FOLLOWED_DID), (
        f"the reposted post rests under its original author {FOLLOWED_DID} "
        f"({_bluesky_actor_hex(FOLLOWED_DID)}); got {author} "
        f"(the reposter {REPOSTER_DID} is {_bluesky_actor_hex(REPOSTER_DID)})"
    )


@pytest.mark.feature("atproto")
def test_a_self_labelled_bluesky_post_arrives_carrying_its_label_as_a_content_warning(
    bluesky_feed_nest, atproto_far_end,
):
    """A post its author self-labelled rests with the first label as its
    content warning; an unlabelled post beside it carries none."""
    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)

    labelled = "a post its author labelled"
    plain = "a post with no label"
    far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE, rkey="3klabel1",
        text=labelled, labels=("nudity", "graphic-media"),
    )
    far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE, rkey="3kplain1", text=plain,
    )
    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] == 2, (
        f"the poll should store both posts: {report!r}; far end saw {far.requests!r}"
    )

    for text, want in ((labelled, "nudity"), (plain, None)):
        wire = _local_bluesky_posts(nest, text)
        assert len(wire) == 1, f"the post {text!r} on the wire once; got {wire!r}"
        post = _stored_post(nest, wire[0]["post_id"])
        assert post.get("content_warning") == want, (
            f"the stored post {text!r} should carry content_warning {want!r} "
            f"(the first self-label); got {post.get('content_warning')!r} in {post!r}"
        )
