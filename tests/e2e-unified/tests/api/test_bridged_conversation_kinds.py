"""tier_3 API e2e: the **bridged-conversation family** — Phase G, both
directions, over a real principal session
(``docs/goal/architecture/apps/bridges.md`` § Bridge-kind catalogue → Phase G;
payload semantics ``docs/goal/ui/conversations.md`` § Where logic lives →
*The ``Bridged`` adapter*; the manifest's ``bridge`` block,
``docs/goal/architecture/third-party.md`` § The manifest).

The client is a real ``https`` document served on loopback
(``helpers.client_metadata_server``) whose signed manifest declares a
``bridge`` block, so the nest's block validation runs against a signer that is
not its own code. One journey:

- the client attests an X25519 holder key and asks for
  ``fauna:conversations:bridge``; the owner approves;
- the roster row carries the block, and ``fauna.bridges.list`` lists the
  bridge with its declared glyph;
- the principal reads its own account's recipient key (and is refused
  another's), creates a room, and deposits — a replay is a duplicate;
- the user lists the room (carrying the bridge's key) and reads the deposit
  back byte-for-byte, then replies through ``send``;
- the principal drains the outbox, receipts the item and acks it;
- the user's inbox shows both directions in arrival order, the Sent copy
  under the id ``send`` answered, its receipt recorded;
- ``rooms.open`` matches the declared grammar nest-side: an address it
  rejects refuses, one it admits opens the same room twice.

The nest never opens a byte here, so the "sealed" bodies are opaque test bytes.
"""

import time

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

from clients.ws_rpc_admin_client import RpcCallError
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.client_metadata_server import (
    DOCUMENT_PATH,
    ClientMetadataServer,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.oauth_client import OAuthClient
from tests.api.test_third_party_session import _alice, _refusal_code, _Session, _upgrade

pytestmark = pytest.mark.tier_3

PUBLISHER = "example.com"
SCOPE = "fauna:conversations:bridge"
REDIRECT_URI = "http://127.0.0.1:17776/callback"
FAR_ROOM = "!room:example.org"
PEER = "@bob:example.org"
SELF = "@alice:example.org"

#: The manifest's block — the record's own field names, `encryption` absent.
BRIDGE = {
    "id": "matrix",
    "glyph": "bridge",
    "address_grammar": "^@[^:]+:.+$",
    "capabilities": {
        "supports_attachments": False,
        "supports_markdown": False,
        "supports_reactions": False,
        "supports_message_delete": False,
        "supports_per_message_reply": False,
        "supports_membership_change": True,
        "supports_recipient_selection": False,
        "supports_rename": False,
        "supports_subject": False,
        "delivery_mode": "Async",
    },
}


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    payload = manifest_payload(PUBLISHER, publisher_key, [])
    payload["bridge"] = BRIDGE
    server.documents[DOCUMENT_PATH] = client_document(
        server.client_id(), REDIRECT_URI, SCOPE, manifest_jws=sign_manifest(publisher_key, payload)
    )
    yield server
    server.close()


@pytest.fixture(scope="module")
def consent_nest_env(metadata_server):
    """The shared consent nest's env: its metadata fetcher pointed at
    ``metadata_server``."""
    return metadata_server.nest_env()


def _approve(nest, client):
    """PAR → the browser page → the owner approves → the code is redeemed.
    No grant twin: the arm wraps no key of the account's."""
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    with _alice(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        consent = next((c for c in pending if c["code"] == browser_code), None)
        assert consent, f"no pending consent carries {browser_code!r}: {pending!r}"
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": consent["consent_id"], "approved": True},
        )
        assert resolved["resolved"], resolved
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect and "code=" in redirect, f"approval must release a code: {redirect!r}"
    tokens = client.exchange_code(redirect)
    assert tokens.get("access_token"), tokens
    return tokens


def _ok(session, kind, payload):
    ok, reply = session.call(kind, payload)
    assert ok, f"{kind}: {reply!r}"
    return reply


@pytest.mark.feature("connected-apps")
def test_a_bridge_carries_a_conversation_both_ways_through_the_sealed_mailbox(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
    alice = nest["user"]
    holder = x25519.X25519PrivateKey.generate()
    holder_pub = holder.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=SCOPE,
        holder_x25519=holder_pub,
        client_id_url=metadata_server.client_id(),
    )
    tokens = _approve(nest, client)

    # ── The block consented and carried. ──
    with _alice(nest) as ws:
        rows = [
            p for p in ws.call("fauna.principals.list", {})["principals"]
            if p["client_id"] == client.client_id
        ]
        assert len(rows) == 1, rows
        assert rows[0]["bridge"]["id"] == "matrix", rows[0]
        listed = [b for b in ws.call("fauna.bridges.list", {})["bridges"] if b["id"] == "matrix"]
        assert len(listed) == 1, listed
        assert listed[0]["glyph"] == "bridge" and listed[0]["linked"], listed[0]

    # ── The bridge's side: the key, the room, the deposit. ──
    session = _Session(_upgrade(nest, client, tokens["access_token"]))
    _ok(session, "fauna.bridges.fetch_recipient_mls_pubkey", {"actor_id": alice["actor_id_bytes"]})
    ok, reply = session.call(
        "fauna.bridges.fetch_recipient_mls_pubkey", {"actor_id": b"\x01" * 32}
    )
    assert not ok and _refusal_code(reply) == "fauna.bridges.permission_denied", reply

    upserted = _ok(
        session,
        "fauna.bridges.conversation.room.upsert",
        {"far_room_id": FAR_ROOM, "participants": [PEER], "self_address": SELF},
    )
    assert upserted["created"], upserted
    room_id = bytes(upserted["room_id"])
    deposit = {
        "far_room_id": FAR_ROOM,
        "far_message_id": "$one",
        "sender": PEER,
        "sealed_content": b"sealed-to-alice-1",
        "created_at": 1,
    }
    first = _ok(session, "fauna.bridges.conversation.deposit", deposit)
    again = _ok(session, "fauna.bridges.conversation.deposit", deposit)
    assert not first.get("duplicate") and again["duplicate"], (first, again)
    assert again["id"] == first["id"]

    # ── The user's side: the room, the deposit read back, the reply. ──
    with _alice(nest) as ws:
        rooms = ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]
        assert [bytes(r["room_id"]) for r in rooms] == [room_id], rooms
        room = rooms[0]
        assert bytes(room["bridge_x25519"]) == holder_pub, room
        assert (room["bridge_id"], room["glyph"], room["participants"]) == ("matrix", "bridge", [PEER])
        assert room["capabilities"]["delivery_mode"] == "Async", room
        inbox = ws.call("fauna.bridges.conversation.inbox.fetch", {"room_id": room_id})
        assert [bytes(m["sealed_content"]) for m in inbox["messages"]] == [b"sealed-to-alice-1"], inbox
        sent = ws.call(
            "fauna.bridges.conversation.send",
            {"room_id": room_id, "sealed_for_bridge": b"sealed-to-bridge", "sealed_for_self": b"sealed-to-alice-2"},
        )
        assert sent["self_address"] == SELF, sent

    # ── The bridge drains, receipts, acks. ──
    items = _ok(session, "fauna.bridges.conversation.outbox.fetch", {})["items"]
    assert [(i["id"], bytes(i["ciphertext"]), i["far_room_id"]) for i in items] == [
        (sent["id"], b"sealed-to-bridge", FAR_ROOM)
    ], items
    _ok(session, "fauna.bridges.conversation.receipt", {"id": sent["id"], "state": "delivered"})
    assert _ok(session, "fauna.bridges.conversation.outbox.ack", {"ids": [sent["id"]]})["acked"] == 1
    assert _ok(session, "fauna.bridges.conversation.outbox.fetch", {})["items"] == []
    session.ws.close()

    # ── Both directions, in arrival order; the grammar matched nest-side. ──
    with _alice(nest) as ws:
        rows = ws.call("fauna.bridges.conversation.inbox.fetch", {"room_id": room_id})["messages"]
        assert [(m["outbound"], bytes(m["sealed_content"])) for m in rows] == [
            (False, b"sealed-to-alice-1"),
            (True, b"sealed-to-alice-2"),
        ], rows
        assert rows[1]["id"] == sent["id"] and rows[1]["receipt"] == "delivered", rows[1]
        with pytest.raises(RpcCallError) as refused:
            ws.call("fauna.bridges.conversation.rooms.open", {"address": "not an address"})
        assert refused.value.code == "fauna.bridges.address_refused", refused.value
        opened = ws.call("fauna.bridges.conversation.rooms.open", {"address": "@carol:example.org"})
        reopened = ws.call(
            "fauna.bridges.conversation.rooms.open",
            {"bridge_id": "matrix", "address": "@carol:example.org"},
        )
        assert bytes(opened["room"]["room_id"]) == bytes(reopened["room"]["room_id"])
        assert opened["room"]["participants"] == ["@carol:example.org"], opened
