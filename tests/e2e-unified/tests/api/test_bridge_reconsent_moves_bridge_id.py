"""tier_3 API e2e: **a re-consent that moves the bridge id disconnects the old
rooms** (``docs/goal/architecture/apps/bridges.md`` § Bridge-kind catalogue
(caller classes) → *When the bridge stops serving*, rules 1 and 2: a bridged
room belongs to ``(account, bridge id)``, and the principal serving it is
whichever live roster row declares that id).

The in-process pin is
``bridged_conversation_handlers::tests::a_reconsent_that_moves_the_bridge_id_disconnects_the_old_rooms``;
this is the same journey against a live nest, a real ``https`` client
document and a real ceremony:

- the client's signed manifest declares the bridge id ``matrix``; the owner
  approves; the principal opens a room and deposits a message;
- the publisher re-publishes the SAME document, its manifest now declaring
  ``signal``, and the owner approves it again — the same principal, now
  serving another network;
- the owner's ``rooms.list`` still carries the ``matrix`` room, now
  ``disconnected: true``; ``inbox.fetch`` still serves its message; ``send``
  into it is refused ``fauna.bridges.bridge_disconnected``;
- the principal, serving ``signal`` now, cannot deposit into the ``matrix``
  room.

The nest's client-metadata cache would hold the first document for its
positive TTL, so this module's nest starts with the ``test-hooks`` seam
``FAUNA_TEST_CLIENT_METADATA_NO_CACHE`` (``oauth_as_client.rs``): the second
ceremony reads the re-published document at once, with no wait on a clock
(convention 14). The nest never opens a byte here; the sealed bodies are opaque
test bytes.
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
from helpers.principal_session import PrincipalSession, alice_client, refusal_code, upgrade

pytestmark = pytest.mark.tier_3

PUBLISHER = "example.com"
SCOPE = "fauna:conversations:bridge"
REDIRECT_URI = "http://127.0.0.1:17804/callback"
FAR_ROOM = "!room:example.org"
PEER = "@bob:example.org"
SELF = "@alice:example.org"
CAPABILITIES = {
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
}


def _block(bridge_id: str) -> dict:
    """The manifest's ``bridge`` block, declaring ``bridge_id``."""
    return {
        "id": bridge_id,
        "glyph": "bridge",
        "address_grammar": "^@[^:]+:.+$",
        "capabilities": CAPABILITIES,
    }


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    server.publisher_key = ed25519.Ed25519PrivateKey.generate()
    publish(server, "matrix")
    yield server
    server.close()


def publish(server, bridge_id: str) -> None:
    """(Re-)publish the one client document, its manifest declaring
    ``bridge_id``, signed by the same publisher key every time."""
    payload = manifest_payload(PUBLISHER, server.publisher_key, [])
    payload["bridge"] = _block(bridge_id)
    server.documents[DOCUMENT_PATH] = client_document(
        server.client_id(),
        REDIRECT_URI,
        SCOPE,
        manifest_jws=sign_manifest(server.publisher_key, payload),
    )


@pytest.fixture(scope="module")
def consent_nest_env(metadata_server):
    """The shared consent nest's env: its metadata fetcher pointed at
    ``metadata_server``, and no positive caching of a fetched document."""
    return {**metadata_server.nest_env(), "FAUNA_TEST_CLIENT_METADATA_NO_CACHE": "1"}


def _approve(nest, client):
    """PAR → the browser page → the owner approves → the code is redeemed.
    No grant twin: the arm wraps no key of the account's."""
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    with alice_client(nest) as ws:
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


def _roster_row(ws, client_id: str) -> dict:
    rows = [p for p in ws.call("fauna.principals.list", {})["principals"] if p["client_id"] == client_id]
    assert len(rows) == 1, rows
    return rows[0]


@pytest.mark.feature("conversations")
def test_a_reconsent_under_another_bridge_id_leaves_the_old_rooms_readable_and_refused(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
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

    # ── Consented as `matrix`: a room, a deposit. ──
    tokens = _approve(nest, client)
    with alice_client(nest) as ws:
        first = _roster_row(ws, client.client_id)
        assert first["bridge"]["id"] == "matrix", first
    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))
    upserted = _ok(
        session,
        "fauna.bridges.conversation.room.upsert",
        {"far_room_id": FAR_ROOM, "participants": [PEER], "self_address": SELF},
    )
    room_id = bytes(upserted["room_id"])
    _ok(
        session,
        "fauna.bridges.conversation.deposit",
        {
            "far_room_id": FAR_ROOM,
            "far_message_id": "$one",
            "sender": PEER,
            "sealed_content": b"sealed-to-alice-1",
            "created_at": 1,
        },
    )
    session.ws.close()
    with alice_client(nest) as ws:
        rooms = ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]
        assert [(bytes(r["room_id"]), r.get("disconnected", False)) for r in rooms] == [
            (room_id, False)
        ], f"the control: the room is connected while `matrix` is served: {rooms!r}"

    # ── The same document re-published declaring `signal`, approved again. ──
    publish(metadata_server, "signal")
    moved_tokens = _approve(nest, client)
    with alice_client(nest) as ws:
        moved = _roster_row(ws, client.client_id)
        assert moved["bridge"]["id"] == "signal", moved
        assert moved["principal_id"] == first["principal_id"], (
            f"a re-consent keeps the principal: {first!r} → {moved!r}"
        )

        # ── The old room: listed disconnected, readable, never writable. ──
        rooms = ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]
        assert [bytes(r["room_id"]) for r in rooms] == [room_id], rooms
        room = rooms[0]
        assert room.get("disconnected") is True and room["bridge_id"] == "matrix", (
            f"no live row declares `matrix` any more: {room!r}"
        )
        inbox = ws.call("fauna.bridges.conversation.inbox.fetch", {"room_id": room_id})
        assert [bytes(m["sealed_content"]) for m in inbox["messages"]] == [b"sealed-to-alice-1"], (
            f"a disconnected room is served as ever: {inbox!r}"
        )
        with pytest.raises(RpcCallError) as refused:
            ws.call(
                "fauna.bridges.conversation.send",
                {
                    "room_id": room_id,
                    "sealed_for_bridge": b"sealed-to-bridge",
                    "sealed_for_self": b"sealed-to-alice-2",
                },
            )
        assert refused.value.code == "fauna.bridges.bridge_disconnected", refused.value

    # ── The principal now serving `signal` reaches no `matrix` room. ──
    moved_session = PrincipalSession(upgrade(nest, client, moved_tokens["access_token"]))
    ok, reply = moved_session.call(
        "fauna.bridges.conversation.deposit",
        {
            "far_room_id": FAR_ROOM,
            "far_message_id": "$two",
            "sender": PEER,
            "sealed_content": b"sealed-to-alice-3",
            "created_at": 2,
        },
    )
    assert not ok and refusal_code(reply) == "fauna.bridges.not_found", reply
    with alice_client(nest) as ws:
        inbox = ws.call("fauna.bridges.conversation.inbox.fetch", {"room_id": room_id})
        assert [bytes(m["sealed_content"]) for m in inbox["messages"]] == [b"sealed-to-alice-1"], (
            f"nothing entered the matrix room through the moved principal: {inbox!r}"
        )
