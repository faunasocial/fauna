"""A cross-post that mentions a member who has not linked Bluesky carries no
Bluesky mention of them, so nothing of theirs is published there.

`docs/goal/architecture/nest/network-exposure.md` § Rulings, F2: a Bluesky
cross-post that mentions a non-linked user omits the mention facet rather than
emitting the mentioned user's raw actor pubkey to the world-readable firehose.
The nest's write-through (`bluesky::write_through_create_inner`) builds the
record with `fauna_bridge_atproto::outbound::fauna_post_to_bsky_record`, whose
`translate_facet` drops every `FacetFeature::Mention` and keeps the display
text; the Rust pin is `mention_facet_is_omitted_never_leaks_actor_pubkey`. This
is the same outcome through a running nest, witnessed at the linked account's
far end.

**The journey.** On a `bluesky` nest whose consume-side OAuth client talks to
:class:`helpers.atproto_fakes.FakeAtprotoFarEnd`, the author links Bluesky over
their own ``fauna.bridges.link`` and the real callback (the fixtures and the
link helper are `test_bluesky_feed_ingest.py`'s, as
`test_bluesky_linked_account.py` imports them), turns cross-posting to
automatic, and writes a post that mentions a second local member who never
linked Bluesky. The far end records each ``createRecord`` body; the test reads
the one carrying the post's text.

**The positive control is a link facet, not a linked member's mention.** A
mention of a member who HAS linked Bluesky would be the natural control, but
the ruling's implementation omits every mention today — the nest has no
identity-mapping layer that resolves a mentioned member's own, consented
Bluesky DID (the comment on `translate_facet` in `outbound.rs`) — so there is
no mention facet for any member to observe. What stops this test passing
vacuously is the same post's link facet: it must cross as an
``app.bsky.richtext.facet#link``, proving facets reach the record at all and
the mention's absence is the omission, not a record that never carries facets.

Witness of ``atproto`` outcome 53.

tier_3: a locally-built nest; the atproto network is a fake.
"""

from __future__ import annotations

import json
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.waiting import wait_until
from tests.api.bare import sign_and_encode_post
from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    _link_through_callback,
    atproto_far_end,
    bluesky_feed_nest,
)

pytestmark = pytest.mark.tier_3

#: `write_through` 1 — automatic cross-posting (`bluesky/bridge_provider.rs`).
AUTO = 1

#: A fire-and-forget write-through leg reaching the far end.
REACH_S = 30.0

POST_COLLECTION = "app.bsky.feed.post"
MENTION_TYPE = "app.bsky.richtext.facet#mention"
LINK_TYPE = "app.bsky.richtext.facet#link"

LINK_URI = "https://example.org/fauna-e2e-unlinked-mention"


def _client(nest, user: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    )


def _bluesky_linked(nest, user: dict) -> bool:
    with _client(nest, user) as client:
        bridges = client.call("fauna.bridges.list", {})["bridges"]
    return next(b for b in bridges if b["id"] == "bluesky")["linked"]


def _published(far, text: str) -> list[dict]:
    return [
        r for r in far.created_snapshot()
        if r.get("collection") == POST_COLLECTION
        and (r.get("record") or {}).get("text") == text
    ]


@pytest.mark.feature("atproto")
def test_a_cross_post_carries_no_bluesky_mention_of_a_member_who_did_not_link(
    bluesky_feed_nest, atproto_far_end,
):
    nest, far = bluesky_feed_nest, atproto_far_end
    from conftest import _make_user

    author = nest["user"]
    mentioned = _make_user(nest)
    mentioned_hex = mentioned["actor_id_hex"].lower()

    _link_through_callback(nest, far, author)
    assert _bluesky_linked(nest, author), "precondition: the author linked Bluesky"
    assert not _bluesky_linked(nest, mentioned), (
        "precondition: the mentioned member has NOT linked Bluesky"
    )
    with _client(nest, author) as client:
        client.call("fauna.bridges.set_settings", {
            "bridge_id": "bluesky", "settings": {"write_through": AUTO},
        })

    # The mention's display text, then a link — byte ranges over the UTF-8
    # text, as `Facet` carries them.
    display = "@unlinked-member"
    text = f"hello {display} see {LINK_URI}"
    raw = text.encode()
    m_start = raw.index(display.encode())
    l_start = raw.index(LINK_URI.encode())
    body = sign_and_encode_post(
        author["signing_key"], int(time.time() * 1_000_000), text,
        facets=[
            {
                "byte_start": m_start,
                "byte_end": m_start + len(display.encode()),
                "feature": {"Mention": {"actor_id": bytes.fromhex(mentioned_hex)}},
            },
            {
                "byte_start": l_start,
                "byte_end": l_start + len(LINK_URI.encode()),
                "feature": {"Link": {"uri": LINK_URI}},
            },
        ],
    )
    with _client(nest, author) as client:
        client.call("fauna.posts.create", {"body": body})

    published = wait_until(
        lambda: _published(far, text), REACH_S,
        diagnose=lambda: (
            f"the cross-post never reached the linked account: no createRecord "
            f"carried {text!r}; createRecord bodies seen: "
            f"{far.created_snapshot()!r}; far end saw {far.requests!r}"
        ),
    )
    assert len(published) == 1, f"the post must be cross-posted once: {published!r}"
    sent = published[0]
    record = sent.get("record") or {}
    wire = json.dumps(sent, sort_keys=True)

    # The display text survives the omission.
    assert record.get("text") == text, f"the post's text must cross whole: {sent!r}"

    features = [
        f for facet in record.get("facets") or []
        for f in facet.get("features") or []
    ]
    # Positive control: facets do reach the record — the link crosses.
    links = [f for f in features if f.get("$type") == LINK_TYPE]
    assert [f.get("uri") for f in links] == [LINK_URI], (
        f"the link facet must cross, or this record never carries facets and the "
        f"mention's absence proves nothing: {sent!r}"
    )

    # The subject: nothing of the unlinked member reaches Bluesky.
    mentions = [f for f in features if f.get("$type") == MENTION_TYPE]
    assert not mentions, (
        f"a cross-post must carry no Bluesky mention of a member who did not "
        f"link Bluesky; mention features {mentions!r} in {sent!r}"
    )
    assert "#mention" not in wire, f"no mention facet of any shape: {wire}"
    assert mentioned_hex not in wire.lower(), (
        f"the mentioned member's actor key must never reach the firehose: {wire}"
    )
    assert not any("did" in f for f in features), (
        f"no facet may name a DID for the mentioned member: {features!r}"
    )
