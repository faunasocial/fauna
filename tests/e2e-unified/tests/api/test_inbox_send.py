"""E2E tier_3 test: `fauna.inbox.send` local delivery of a Python-composed
signed `(ContactRequest, Post)` email tuple.

`fauna.inbox.send` is the WS-RPC successor of the deleted
`POST /api/v1/inbox/{actor}` twin. The Rust
tier_3 `bins/fauna-nest/tests/conformance_inbox.rs` is the primary guarantee,
but it composes the payload with the *same* Rust encoder it verifies against,
and uses a `group/v1` payload — which takes the `is_group_payload` bypass and
delivers regardless of `InboxMode`. This test adds the first *cross-language*
proof of the wire contract: an independently Python-composed `email/v1`
`(cr, post)` tuple — byte-built from the serialization spec, not the Rust
composer — is accepted by `fauna.inbox.send`, local-delivered under the
recipient's `open` inbox mode, and read back verbatim via `fauna.inbox.fetch`.
That exercises the *email* delivery path the Rust conformance bypasses and
guards against wire-shape drift between the spec and the Rust encoder.

Wire shapes (`libs/fauna-protocol/src/inbox.rs`):
  - fauna.inbox.send  -> {recipient_actor_id: hex, recipient_nest_url: None|url,
                          payload_bytes: bstr} -> {inbox_id: int|None}
  - fauna.inbox.fetch -> {limit: int} -> {items: [{id, payload}], more}
  - fauna.inbox.ack   -> {ids: [int]} -> {acked: int}

The email tuple is built by `common.build_email_inbox_payload` — the Python
mirror of `fauna_client_core::email::build_signed_email`.
"""
import pytest

from common import build_email_inbox_payload, create_actor_and_register
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

pytestmark = pytest.mark.tier_3


def _client(node_url, actor) -> WsRpcAdminClient:
    """A WS-RPC client authed as a registered user actor. `fauna.inbox.*` are
    User-class kinds, so a plain registered actor (not just admin) can drive
    them; `send` is caller-scoped to this actor."""
    return WsRpcAdminClient(
        node_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def test_inbox_send_email_local_delivery_roundtrip(nest_instance):
    """A Python-composed email `(cr, post)` tuple sent via `fauna.inbox.send`
    is local-delivered to a same-nest recipient (open mode), fetched back
    byte-identically, then drained by `ack`."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    sender = create_actor_and_register(port, admin_signing_key=admin_sk)
    recipient = create_actor_and_register(port, admin_signing_key=admin_sk)

    # Open the recipient's inbox so an email from a non-contact delivers — the
    # default `allow_knock` would hold it as a knock (inbox_id=None) instead.
    with _client(node_url, recipient) as rc:
        rc.call("fauna.inbox.mode.set", {"mode": "open"})

    payload, _post_id = build_email_inbox_payload(
        sender["signing_key"],
        recipient["actor_id_hex"],
        "Hello over the wire",
        "Composed in Python, verified by the nest.",
        node_url=node_url,
    )

    # The sender hands its home nest the signed tuple; None recipient_nest_url
    # => same nest as the caller's home nest => local delivery (no federation).
    with _client(node_url, sender) as sc:
        reply = sc.call(
            "fauna.inbox.send",
            {
                "recipient_actor_id": recipient["actor_id_hex"],
                "recipient_nest_url": None,
                "payload_bytes": payload,
            },
        )
    assert reply.get("inbox_id") is not None, (
        f"an open-mode email must local-deliver to an inbox row, got {reply!r}"
    )

    # The recipient drains its inbox: exactly one item. The stored payload is the
    # canonical inbox envelope (layer 1) — a ContactRequest envelope whose payload
    # is the signed tuple verbatim, so the exact bytes the Python encoder sent are
    # embedded contiguously inside the fetched envelope.
    with _client(node_url, recipient) as rc:
        fetched = rc.call("fauna.inbox.fetch", {"limit": 32})
        items = fetched["items"]
        assert len(items) == 1, f"expected one delivered item, got {items!r}"
        assert payload in items[0]["payload"], (
            "the inbox envelope must embed the exact tuple the Python encoder sent"
        )
        assert not fetched["more"]

        # fetch is a peek; ack is what consumes (the data-loss-fix split).
        acked = rc.call("fauna.inbox.ack", {"ids": [items[0]["id"]]})
        assert acked["acked"] == 1
        drained = rc.call("fauna.inbox.fetch", {"limit": 32})
        assert drained["items"] == [], f"ack must drain the queue, got {drained!r}"


def test_inbox_send_rejects_non_sender_caller(nest_instance):
    """Sender-binding over the wire: a registered actor that did NOT sign the
    tuple cannot relay it — `fauna.inbox.send` enforces `cr.sender == caller`
    (the security property the unauthenticated HTTP twin could not). Proven
    here end-to-end from an independent Python encoder."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    sender = create_actor_and_register(port, admin_signing_key=admin_sk)
    imposter = create_actor_and_register(port, admin_signing_key=admin_sk)
    recipient = create_actor_and_register(port, admin_signing_key=admin_sk)

    # A valid tuple SIGNED BY `sender` (the payload itself is well-formed) ...
    payload, _ = build_email_inbox_payload(
        sender["signing_key"],
        recipient["actor_id_hex"],
        "Spoof attempt",
        "body",
        node_url=node_url,
    )

    # ... but relayed by `imposter`, a different authed caller => denied.
    with _client(node_url, imposter) as ic:
        with pytest.raises(RpcCallError) as ei:
            ic.call(
                "fauna.inbox.send",
                {
                    "recipient_actor_id": recipient["actor_id_hex"],
                    "recipient_nest_url": None,
                    "payload_bytes": payload,
                },
            )
    assert ei.value.code == "fauna.inbox.permission_denied", (
        f"a non-sender caller must be denied, got {ei.value.code!r}"
    )
