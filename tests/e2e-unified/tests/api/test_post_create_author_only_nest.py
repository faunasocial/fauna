"""tier_3 nest-side witness for author-only post upload (`feed.md` § Post creation).

`docs/features/feed-compose.md` outcome 8: the nest stores a post only from the
connection of the person who wrote it. A validly signed post uploaded over
anyone else's connection is refused (`classify_encrypted_post` →
`PostUploaderNotAuthor`, `bins/fauna-nest/src/storage/sealed.rs`) and nothing of
it is kept.

Production flow asserted: the stranger's `fauna.posts.create` →
`posts_handlers::posts_create_handler` → `ingest_post_core` → classify refuses
`post_uploader_not_author` → `fauna.posts.ingest_failed` carrying that reason →
the feed answers as if the post never existed. Every assertion is a state read
after a call the nest has already answered (convention 14).
"""

import time

import pytest
from clients.ws_rpc_admin_client import RpcCallError
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("feed-compose")
def test_a_post_uploaded_by_anyone_but_its_author_is_refused_and_not_kept(two_nodes):
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    stranger = create_actor_and_register(port, admin_signing_key=admin_sk)

    # Validly signed by the author — only the uploading connection is wrong.
    text = "Not yours to upload"
    wrapped = sign_and_encode_post(author["signing_key"], int(time.time() * 1_000_000), text)

    with pytest.raises(RpcCallError) as refused:
        ws_api.create_post(port, stranger, wrapped)
    assert refused.value.code == "fauna.posts.ingest_failed", refused.value.code
    assert "post_uploader_not_author" in str(refused.value.details), refused.value.details

    # Nothing of it is kept, in anyone's feed.
    for who in (author, stranger):
        assert not any(text in str(p) for p in ws_api.local_feed_posts(port, who)), (
            "a refused upload must leave nothing in the feed"
        )

    # Positive control: the same bytes from the author's own connection are
    # stored, so the refusal above is about the uploader and nothing else.
    post_id = ws_api.create_post(port, author, wrapped)
    assert ws_api.get_post(port, author, post_id), "the author's own upload is stored"
