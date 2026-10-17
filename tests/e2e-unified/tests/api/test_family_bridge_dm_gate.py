"""The guardian's refusal of a bridged-DM peer, observed at the bridge — tier_3 API E2E.

Witnesses `docs/features/family-safety.md` outcome **20** — *"Once the guardian
refuses someone who messaged the child through a linked account on another
network, that person's new messages never arrive"*
(`docs/goal/behavior/family-safety.md` § The bridge-DM gate).

The bridge is a real third-party principal (the same consent ceremony and DPoP
session `test_bridged_conversation_kinds.py` drives) riding a supervised
account whose guardian holds DMs from unknown peers. A cold peer's first
message lands held and puts the peer in the guardian's queue; the guardian
refuses the peer; the peer's next message is refused at the bridge's deposit
door, so it never reaches the child. The discriminator is a second cold peer
on the same bridge and room shape: its message is still taken, so the refusal
is the guardian's verdict on that one person, not a bridge that stopped
delivering.

The nest never opens a byte of a deposit, so the "sealed" bodies are opaque
test bytes.

Process safety: no ``pkill``/``killall``; the consent nest is a module fixture.
"""

from __future__ import annotations

import os
import time

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    USER_ROTATION_PUB,
    consent_bridge,
    consent_nest,
)
from helpers.client_metadata_server import (
    DOCUMENT_PATH,
    ClientMetadataServer,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.family_wire import (
    actor_ws,
    admit_adult_directly,
    admit_ward,
    blocked_dm_peers,
    decide_dm_peer,
    set_ward_policy,
)
from helpers.oauth_client import OAuthClient
from tests.api.test_bridged_conversation_kinds import _approve
from helpers.principal_session import PrincipalSession, refusal_code, upgrade

pytestmark = pytest.mark.tier_3

PUBLISHER = "example.com"
SCOPE = "fauna:conversations:bridge"
REDIRECT_URI = "http://127.0.0.1:17783/callback"
SELF = "@kid:example.org"
REFUSED_PEER = "@refused:example.org"
OTHER_PEER = "@unknown:example.org"
#: `bridged_conversation_handlers.rs` — a deposit from a peer the guardian refused.
BRIDGES_GUARDIAN_APPROVAL = "fauna.bridges.guardian_approval_required"

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
        "supports_membership_change": False,
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
    """The consent nest's env: its metadata fetcher pointed at ``metadata_server``."""
    return metadata_server.nest_env()


def _raw(public_key) -> bytes:
    return public_key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def _held_ward(nest) -> tuple[dict, dict]:
    """A guardian and a supervised account on the consent nest, the guardian
    holding DMs from unknown peers. The ward is taken to ``hosted_full``, as
    the consent nest's own user is, because the bridge's consent ceremony
    completes as an identity. One anonymous invite-request submit."""
    tail = os.urandom(3).hex()
    guardian = admit_adult_directly(nest, f"dmgate-guardian-{tail}")
    ward = admit_ward(nest, guardian, f"dmgate-ward-{tail}")
    ward["actor_id_bytes"] = bytes.fromhex(ward["actor_id_hex"])
    set_ward_policy(nest, guardian, ward, unknown_peer_dm="hold")
    with actor_ws(nest, ward) as ws:
        ws.call(
            "fauna.bridges.atproto.set_integration_level",
            {
                "target_level": "hosted_full",
                "did_method": "plc",
                "user_rotation_pub_did_key": USER_ROTATION_PUB,
                "history_backfill": False,
            },
        )
    return guardian, ward


def _bridge_session(nest, metadata_server) -> PrincipalSession:
    """The bridge principal, consented by ``nest["user"]``."""
    holder = x25519.X25519PrivateKey.generate()
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=SCOPE,
        holder_x25519=_raw(holder.public_key()),
        client_id_url=metadata_server.client_id(),
    )
    tokens = _approve(nest, client)
    return PrincipalSession(upgrade(nest, client, tokens["access_token"]))


def _deposit(session: PrincipalSession, *, far_room_id: str, far_message_id: str, sender: str):
    """The bridge's inbound leg: the room, then one opaque message. Returns the
    deposit's ``(ok, reply)``; the room upsert must succeed."""
    ok, reply = session.call(
        "fauna.bridges.conversation.room.upsert",
        {"far_room_id": far_room_id, "participants": [sender], "self_address": SELF},
    )
    assert ok, f"room.upsert for {sender} was refused: {reply!r}"
    return session.call(
        "fauna.bridges.conversation.deposit",
        {
            "far_room_id": far_room_id,
            "far_message_id": far_message_id,
            "sender": sender,
            "sealed_content": os.urandom(48),
            "created_at": int(time.time() * 1000),
        },
    )


def _room(nest, ward: dict, far_room_id: str) -> dict:
    with actor_ws(nest, ward) as ws:
        rooms = ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]
    room = next((r for r in rooms if r["far_room_id"] == far_room_id), None)
    assert room is not None, f"no room {far_room_id!r} on the ward's side: {rooms!r}"
    return room


def _room_messages(nest, ward: dict, room: dict) -> list:
    with actor_ws(nest, ward) as ws:
        return ws.call(
            "fauna.bridges.conversation.inbox.fetch", {"room_id": room["room_id"]}
        )["messages"]


def _dm_holds(nest, guardian: dict, ward: dict) -> list[str]:
    """The peer addresses in the guardian's queue as held bridged DMs."""
    with actor_ws(nest, guardian) as ws:
        approvals = ws.call("fauna.family.approvals.list", {}).get("approvals") or []
    want = ward["actor_id_bytes"]
    return [
        a["peer_address"]
        for a in approvals
        if a.get("kind") == "dm_hold" and bytes(a.get("supervised_actor_id") or b"") == want
    ]


@pytest.mark.feature("family-safety")
def test_a_bridged_peer_the_guardian_refused_never_gets_another_message_through(
    consent_nest, consent_bridge, metadata_server
):
    """Outcome 20: after the guardian's no, the refused peer's next message is
    refused at the deposit door, while another unknown peer is still taken."""
    guardian, ward = _held_ward(consent_nest)
    nest = {**consent_nest, "user": ward}
    session = _bridge_session(nest, metadata_server)
    try:
        refused_room = f"!refused-{os.urandom(3).hex()}:example.org"
        ok, reply = _deposit(
            session, far_room_id=refused_room, far_message_id="$first", sender=REFUSED_PEER
        )
        assert ok, f"a cold peer's first message must be taken and held: {reply!r}"
        room = _room(consent_nest, ward, refused_room)
        assert room.get("guardian_state") == "held", room
        assert REFUSED_PEER in _dm_holds(consent_nest, guardian, ward), (
            "the cold peer must be waiting in the guardian's queue before the no"
        )

        with actor_ws(consent_nest, guardian) as guardian_ws:
            decide_dm_peer(guardian_ws, ward["actor_id_bytes"], "matrix", REFUSED_PEER, False)
            refused = blocked_dm_peers(guardian_ws, ward["actor_id_bytes"])
        assert {p["peer_id"] for p in refused} == {REFUSED_PEER}, refused

        ok, reply = _deposit(
            session, far_room_id=refused_room, far_message_id="$second", sender=REFUSED_PEER
        )
        assert not ok and refusal_code(reply) == BRIDGES_GUARDIAN_APPROVAL, (
            f"the refused peer's new message must be refused with "
            f"{BRIDGES_GUARDIAN_APPROVAL}; got ok={ok} {reply!r}"
        )
        assert len(_room_messages(consent_nest, ward, room)) == 1, (
            "only the message taken before the guardian's no may be in the room"
        )
        assert _room(consent_nest, ward, refused_room).get("guardian_state") == "blocked"

        other_room = f"!other-{os.urandom(3).hex()}:example.org"
        ok, reply = _deposit(
            session, far_room_id=other_room, far_message_id="$hello", sender=OTHER_PEER
        )
        assert ok, (
            f"another unknown peer on the same bridge must still be taken — the "
            f"refusal is about the refused person alone: {reply!r}"
        )
    finally:
        session.ws.close()
