"""E2E tests for the unified post-interaction surface ``fauna.posts.interact``.

Verifies that the handler correctly routes interactions and surfaces the
same error semantics the deleted ``POST /api/v1/posts/{id}/interact`` HTTP
twin did, now over WS-RPC (T4):

- invalid action → ``fauna.posts.invalid_params`` (was HTTP 400)
- non-hex post_id → ``fauna.posts.invalid_params`` (was HTTP 400)
- unknown post → ``fauna.posts.not_found`` (was HTTP 404)
- native fauna post like → ``{"ok": true}`` (was HTTP 200)

The error codes map from the shared ``interact_with_post_core`` ApiError
status in ``bins/fauna-nest/src/posts_handlers.rs::rpc_error_from_api``
(400 → invalid_params, 404 → not_found).
"""

import json
import time

import pytest
from common import create_actor_and_register

from clients.ws_rpc_admin_client import RpcCallError
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


def test_invalid_action_raises_invalid_params(two_nodes):
    """Actions must be one of: like, unlike, reply, repost, unrepost, quote."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    # A syntactically valid (64 hex chars = 32 bytes) but unknown post ID;
    # action validation runs before the post lookup, so the invalid action
    # is what's reported.
    post_id = "a" * 64
    with pytest.raises(RpcCallError) as exc:
        ws_api.interact(port, actor, post_id, action="follow")
    assert exc.value.code == "fauna.posts.invalid_params", exc.value.code


def test_invalid_post_id_raises_invalid_params(two_nodes):
    """Post ID must be valid 32-byte hex."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    with pytest.raises(RpcCallError) as exc:
        ws_api.interact(port, actor, "not-hex", action="like")
    assert exc.value.code == "fauna.posts.invalid_params", exc.value.code


def test_missing_post_raises_not_found(two_nodes):
    """A post that doesn't exist should map to fauna.posts.not_found."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    post_id = "ff" * 32
    with pytest.raises(RpcCallError) as exc:
        ws_api.interact(port, actor, post_id, action="like")
    assert exc.value.code == "fauna.posts.not_found", exc.value.code


@pytest.mark.feature("feed-interactions")
def test_fauna_post_like_returns_ok(two_nodes):
    """Liking a native Fauna post should succeed with {"ok": true}."""
    port = two_nodes["port_a"]
    author = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    liker = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    # Seed a real native fauna post to like.
    now_us = int(time.time() * 1_000_000)
    post_bytes = sign_and_encode_post(author["signing_key"], now_us, "A likeable post")
    post_id = ws_api.create_post(port, author, post_bytes)

    reply = ws_api.interact(port, liker, post_id, action="like")
    assert reply["source"] == "fauna", reply
    # `result` is a JSON string round-tripping the heterogeneous HTTP-twin
    # reply bodies; the native like body was `{"ok": true}`.
    assert json.loads(reply["result"]).get("ok") is True, reply
